//! **Stores as machines** (restructure spec §2.1/§2.2, phase 4; `docs/stores-as-machines.md`).
//!
//! Seven data modules provide the application's server-derived state — `browse`, `pms` (the Home
//! hubs), `metadata`, `search`, `person`, `collection`, `viewstate`. All seven are physically owned by one
//! [`Stores`] aggregate per `crate::app::bridge::Bridge`, each with its own per-instance state,
//! adapter and notice (Browse/Person/ViewState landed first; Search, Hubs and Metadata completed
//! the port). This layer puts ONE entrance in front of each: a [`StoreCmd`] is the complete,
//! enumerated vocabulary of mutations, a store's owned command decoder is the one place its
//! vocabulary is applied, and every command that changes observable state or landing that changes
//! the store raises the store's NOTICE (a generation the bridge dispatcher delivers to every live
//! instance as `ScreenEvent::StoreChanged`, spec §3.4).
//!
//! Owned stores have two caller shapes and one explicit owner: screens emit `AppFx::Store(id, cmd)` for
//! `app/bridge.rs` to deliver, while same-turn application boundaries call a method on the
//! [`Stores`] value they already hold. There is no generic `apply(cmd)` dispatcher any more — every
//! store's vocabulary is applied only through its own owner.
//!
//! What lives here is the vocabulary and machines plus all seven stores' production aggregate; no
//! data stays in a legacy compatibility global any more (§14 complete). This module names data
//! crates, `plx_machine::machine` and — since
//! phase 11's landing schedule — `plx_machine::landgate`, and nothing else (spec §2.1's layer rule;
//! `ci/check-deps.sh`'s `mutators` gate refuses the old spelling outside `stores/` and the data
//! modules).

use plx_machine::machine::StoreOrd;

// The advisory endpoint-refresh request, its first-observation-ordered set, and the host trait that
// turns one into an effect live in `plex::retry`: the plaintext grant's upgrade retry (`plex`) answers
// with the same set the data layer's outcomes carry, and `plex` cannot name this module. Re-exported
// so every store, adapter and screen keeps its spelling; `StoreEffectHost` is the name the stores'
// machines are written against.
pub use plx_plex::plex::retry::{EndpointRefresh, EndpointRefreshSet};
pub use plx_plex::plex::retry::EndpointRefreshHost as StoreEffectHost;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[must_use]
pub struct StoreOutcome {
    pub changed: bool,
    pub endpoints: EndpointRefreshSet,
}

impl StoreOutcome {
    pub fn changed(changed: bool) -> Self { Self { changed, ..Self::default() } }
}

/// The items a page shows, as the stores that hold them name them: each store's `held_keys`
/// returns the `(server, ratingKey)` of the rows inside its own hold window (Home's `hold`, a
/// Library's shelf hold and grid `want`, a result set's published rows). Layer 2's re-reads
/// (`app::reread`) hand them to `ViewReconcileStore::enqueue`; nothing on `plx_ui::Screen` knows
/// them. Placeholders (no rating key) are never listed, and an item appears once however many rows
/// hold it.
pub type HeldKeys = Vec<(plx_plex::plex::ServerId, String)>;

/// Add `(sid, rk)` to `keys` unless it is a placeholder or is already listed.
pub(crate) fn push_held(keys: &mut HeldKeys, sid: plx_plex::plex::ServerId, rk: &str) {
    if rk.is_empty() || keys.iter().any(|(s, k)| plx_plex::plex::same_item((*s, k), (sid, rk))) {
        return;
    }
    keys.push((sid, rk.to_owned()));
}

pub mod browse;
pub mod catalog;
pub mod fanout;
mod content_arg;
pub use content_arg::ContentArg;
pub mod hubs;
pub mod metadata;
pub mod notify;
pub mod notify_map;
pub mod person;
pub mod collection;
pub(crate) mod deck;
pub(crate) mod deck_group;
pub mod page_cache;
pub mod paging;
pub mod search;
pub mod subsearch;
pub mod tape;
pub mod viewreconcile;
pub mod viewstate;
pub mod watchlist;

/// Production store aggregate. All seven stores are physical owners here — Browse, Person and
/// ViewState landed first; Search, Hubs and Metadata completed the port.
pub struct Stores {
    /// The landing schedule belongs to the same application owner as these stores. Two Bridges
    /// may contain the same `StoreId`; they must not share its replay cursor or wait budget.
    pub landgate: plx_machine::landgate::Gate,
    pub browse: std::rc::Rc<std::cell::RefCell<browse::BrowseStore>>,
    pub hubs: hubs::HubsStore,
    pub metadata: metadata::MetadataStore,
    pub person: person::PersonStore,
    pub collection: collection::CollectionStore,
    pub search: search::SearchStore,
    /// The player's subtitle search & download (`crate::subsearch`). Appended last.
    pub subtitle_search: subsearch::SubSearchStore,
    pub viewstate: std::cell::RefCell<viewstate::ViewStateStore>,
    /// The read-back of landed view-state writes. Appended after ViewState's other owners.
    pub reconcile: viewreconcile::ViewReconcileStore,
    /// The plex.tv Watchlist (`crate::watchlist`). Appended last, like its `StoreId`.
    pub watchlist: watchlist::WatchlistStore,
    /// The catalog page's discover record (`crate::catalog`). Appended last, like its `StoreId`.
    pub catalog: catalog::CatalogStore,
    /// The PMS notification feed's owner. Ships on for owned servers (`notify::NOTIFY_ENABLED`); no `StoreId`, no
    /// `StoreCmd`: the application pumps it and tells it the lifecycle (`Bridge::notify_*`).
    pub notify: notify::NotifyStore,
    /// What the feed's play-state hints owe the screens (`notify_map`); nothing is read until it is
    /// pumped with the pages on the stack.
    notify_map: notify_map::NotifyMap,
    /// The publish order of view-state edits (`crate::viewledger`): `viewstate` records into it, and
    /// every store that reads stamps a read from it and re-patches the read when it lands. A store
    /// replaced after construction takes it as an argument (`HubsStore::from_parts`).
    pub view_ledger: crate::viewledger::ViewLedger,
    /// The player is on top: view-state reconcile work that reaches the network (the read-back, the
    /// hub refetch a landed write owes, the membership re-query it may start) stays owed instead of
    /// running under the video. Set once a frame by [`Stores::hold_network`].
    network_held: bool,
}

impl Default for Stores {
    fn default() -> Self {
        let browse = std::rc::Rc::new(std::cell::RefCell::new(browse::BrowseStore::default()));
        let mut stores = Self {
            landgate: Default::default(),
            browse,
            hubs: hubs::HubsStore::default(),
            metadata: metadata::MetadataStore::default(),
            person: person::PersonStore::default(),
            collection: collection::CollectionStore::default(),
            search: search::SearchStore::default(),
            subtitle_search: subsearch::SubSearchStore::default(),
            viewstate: std::cell::RefCell::new(viewstate::ViewStateStore::default()),
            reconcile: viewreconcile::ViewReconcileStore::default(),
            watchlist: watchlist::WatchlistStore::default(),
            catalog: catalog::CatalogStore::default(),
            notify: notify::NotifyStore::default(),
            notify_map: Default::default(),
            view_ledger: Default::default(),
            network_held: false,
        };
        stores.share_view_ledger();
        stores
    }
}

impl Stores {
    /// Hand [`Stores::view_ledger`] to every store that reads, once, at construction. A store built
    /// afterwards takes the ledger as a constructor argument instead (`HubsStore::from_parts`).
    fn share_view_ledger(&mut self) {
        let ledger = &self.view_ledger;
        self.browse.borrow_mut().share_ledger(ledger);
        self.hubs.share_ledger(ledger);
        self.metadata.share_ledger(ledger);
        self.person.share_ledger(ledger);
        self.collection.share_ledger(ledger);
        self.search.share_ledger(ledger);
        self.viewstate.borrow_mut().share_ledger(ledger);
        self.reconcile.share_ledger(ledger);
    }

    /// Explicit synchronous Browse command path. The answer is available before this call returns.
    /// `keys` is the registry's answer as of this call. A command may leave a
    /// [`StoreJob::BrowseHubs`] or a [`StoreJob::BrowseDirectory`] on Browse's outbox: the caller that
    /// returns without [`Stores::take_jobs_of`] and handing it to a runner has stranded the read. The one
    /// production caller is `Bridge::browse_run`, which drains in the same call.
    pub fn browse_run(&self, cmd: browse::BrowseCmd, keys: &plx_plex::plex::ClientKeys) -> bool {
        self.browse.borrow_mut().run(cmd, keys)
    }

    /// [`Stores::browse_run`] on a registry that holds no client (no section-hubs or directory read leaves): the
    /// command of a test about something else.
    #[cfg(any(test, feature = "test-support"))]
    pub fn browse_run_for_test(&self, cmd: browse::BrowseCmd) -> bool {
        self.browse_run(cmd, &plx_plex::plex::ClientKeys::default())
    }

    /// Is any shelf of the addressed section committed? (The Library pages' publication gate asks.)
    pub fn browse_hubs_committed(&self, target: browse::SectionAddress) -> bool {
        self.browse.borrow().hubs_committed(target)
    }

    pub fn browse_discover_pump(&self) -> StoreOutcome {
        self.browse.borrow_mut().discover_pump_with_gate(&self.landgate)
    }

    pub fn person_run(&mut self, cmd: person::PersonCmd) -> bool {
        self.person.run(cmd)
    }

    /// Run one Collection command; `keys` is the registry's answer as of this call. A command
    /// emits no job.
    pub fn collection_run(&mut self, cmd: collection::CollectionCmd, keys: &plx_plex::plex::ClientKeys) -> bool {
        self.collection.run(cmd, keys)
    }

    /// Pump the Collection store; `keys` is the registry's answer as of this call. It does NOT start
    /// the request it emitted: a caller that returns without [`Stores::take_jobs_of`] and handing
    /// the job to a runner has stranded the page's fetch. The one production caller is
    /// `Bridge::collection_pump`, which drains in the same call.
    pub fn collection_pump(&mut self, keys: &plx_plex::plex::ClientKeys) -> bool {
        self.collection.pump(&self.landgate, keys)
    }

    pub fn collection_view(&self) -> crate::collection::CollectionView<'_> {
        self.collection.view()
    }

    pub fn subtitle_search_run(&mut self, cmd: subsearch::SubSearchCmd) -> bool {
        self.subtitle_search.run(cmd)
    }

    /// Pump the subtitle-search store; `keys` is the registry's answer as of this call. It does NOT
    /// start the request it emitted: a caller that returns without [`Stores::take_jobs_of`] and
    /// handing the job to a runner has stranded the search. The one production caller is
    /// `Bridge::subsearch_pump`, which drains in the same call.
    pub fn subtitle_search_pump(&mut self, keys: &plx_plex::plex::ClientKeys) -> bool {
        self.subtitle_search.pump(&self.landgate, keys)
    }

    pub fn subtitle_search_view(&self) -> crate::subsearch::SubSearchView<'_> {
        self.subtitle_search.view()
    }

    /// Pump the Person store; `keys` is the registry's answer as of this call. It does NOT start the
    /// requests it emitted: a caller that returns without [`Stores::take_jobs_of`] and handing the
    /// jobs to a runner has stranded the page's fetches. The one production caller is
    /// `Bridge::person_pump`, which drains in the same call.
    pub fn person_pump(&mut self, keys: &plx_plex::plex::ClientKeys) -> bool {
        self.person.pump(&self.landgate, keys)
    }

    pub fn person_view(&self) -> crate::person::PersonView<'_> {
        self.person.view()
    }

    /// Run one Metadata command. It does NOT start the jobs the command emitted: a `RequestDetail`
    /// leaves its fetch on the outbox, and a caller that returns without [`Stores::take_jobs`] and
    /// handing each job to a runner has stranded the request. The one production caller is
    /// `Bridge::metadata_run`, which drains in the same call; the application's test rigs fail a
    /// frame that ends with a job still here (`Bridge::assert_no_store_jobs_left_for_test`).
    pub fn metadata_run(&mut self, cmd: metadata::MetadataCmd) -> bool {
        self.metadata.run(cmd)
    }

    /// Take every job any store has emitted since the last take, store by store in a fixed order.
    /// The caller OWES each one to a runner (see [`StoreJob`]). A store with an outbox joins in
    /// [`Stores::take_jobs_of`] and `has_jobs_of`, and in the `*_FILLS` list of every fanning call
    /// that reaches it ([`Stores::VIEWSTATE_FILLS`], [`Stores::NOTIFY_FILLS`]): the Bridge's backstop
    /// and its leftover-job check both read this aggregate, which walks [`StoreId::ALL`].
    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<StoreJob> {
        StoreId::ALL.into_iter().flat_map(|id| self.take_jobs_of(id)).collect()
    }

    /// Take the jobs ONE store has emitted since the last take, in order, and nothing another
    /// store left behind. A store's own Bridge pump drains with this (`Bridge::start_store_jobs_of`),
    /// right after its step: a job some other sender left on its outbox is then still found, and
    /// counted, by the backstop, instead of being started silently here.
    #[must_use]
    pub fn take_jobs_of(&mut self, id: StoreId) -> Vec<StoreJob> {
        match id {
            StoreId::Metadata => self.metadata.take_jobs(),
            StoreId::Collection => self.collection.take_jobs(),
            StoreId::Person => self.person.take_jobs(),
            StoreId::Catalog => self.catalog.take_jobs(),
            StoreId::SubtitleSearch => self.subtitle_search.take_jobs(),
            StoreId::ViewReconcile => self.reconcile.take_jobs(),
            StoreId::Hubs => self.hubs.take_jobs(),
            StoreId::Browse => self.browse.borrow_mut().take_jobs(),
            StoreId::Search => self.search.take_jobs(),
            StoreId::ViewState => self.viewstate.borrow_mut().take_jobs(),
            // The other stores emit no jobs yet.
            _ => Vec::new(),
        }
    }

    /// Does any store hold an emitted job nobody has taken? Aggregates every outbox, as
    /// [`Stores::take_jobs`] does.
    pub fn has_jobs(&self) -> bool {
        StoreId::ALL.into_iter().any(|id| self.has_jobs_of(id))
    }

    fn has_jobs_of(&self, id: StoreId) -> bool {
        match id {
            StoreId::Metadata => self.metadata.has_jobs(),
            StoreId::Collection => self.collection.has_jobs(),
            StoreId::Person => self.person.has_jobs(),
            StoreId::Catalog => self.catalog.has_jobs(),
            StoreId::SubtitleSearch => self.subtitle_search.has_jobs(),
            StoreId::ViewReconcile => self.reconcile.has_jobs(),
            StoreId::Hubs => self.hubs.has_jobs(),
            StoreId::Browse => self.browse.borrow().has_jobs(),
            StoreId::Search => self.search.has_jobs(),
            StoreId::ViewState => self.viewstate.borrow().has_jobs(),
            // The other stores emit no jobs yet.
            _ => false,
        }
    }

    pub fn metadata_pump(&mut self) -> bool {
        self.metadata.pump(&self.landgate)
    }

    pub fn metadata_view(&self) -> crate::metadata::MetadataView<'_> {
        self.metadata.view()
    }

    pub fn search_run(
        &mut self,
        cmd: search::SearchCmd,
        directory: browse::DirectoryView<'_>,
    ) -> bool {
        self.search.run_with_directory(cmd, directory)
    }

    /// Pump the Search store; `keys` is the registry's answer as of this call. It does NOT start the
    /// requests it emitted: a caller that returns without [`Stores::take_jobs_of`] and handing the
    /// jobs to a runner has stranded the sources' fetches. The one production caller is
    /// `Bridge::search_pump`, which drains in the same call.
    pub fn search_pump(
        &mut self,
        dt: f32,
        directory: browse::DirectoryView<'_>,
        keys: &plx_plex::plex::ClientKeys,
    ) -> bool {
        self.search.pump_with_directory_and_gate(dt, directory, &self.landgate, keys)
    }

    pub fn search_snapshot(
        &self,
        directory: browse::DirectoryView<'_>,
        household: &[i64],
    ) -> search::SearchSnapshot {
        self.search.snapshot_with_directory(directory, household)
    }

    /// Controlled discovery against this aggregate's Browse owner.
    pub fn browse_controlled_discover(
        &self,
        launch: &mut dyn FnMut(crate::browse::DiscoveryRequest) -> bool,
    ) {
        self.browse.borrow_mut().controlled_discover(launch);
    }

    /// The outboxes [`Stores::viewstate_run`] and [`Stores::viewstate_pump`] can fill, in drain
    /// order (`StoreId` order): ViewState's terminal `HubsInvalidateAll` reaches Browse's
    /// section-hubs step, and its `RefetchHubs` and `Reset` reach the Hubs step; its own step sends
    /// the next queued write ([`StoreJob::ViewWrite`]). Its Person, Collection, Search and Metadata
    /// edits emit no jobs from this call. The Bridge drains exactly these after either call
    /// (`Bridge::start_store_jobs_in`).
    pub const VIEWSTATE_FILLS: &'static [StoreId] = &[StoreId::Browse, StoreId::Hubs, StoreId::ViewState];

    /// ViewState's synchronous command path, with every Browse side effect addressed back to this
    /// aggregate. The callback is invoked inline, preserving the press-frame optimistic edit. The
    /// outcome's `endpoints` are the refreshes the edit's Hubs step settled: the caller carries them.
    /// `clients` is the registry's answer as of this call and `profile` the profile the Session owner
    /// has seated (`Bridge::profile_scope`): a write is sent only under the profile it was pressed under.
    pub fn viewstate_run(
        &mut self,
        cmd: viewstate::ViewStateCmd,
        directory: browse::DirectoryView<'_>,
        clients: &plx_plex::plex::ClientKeys,
        profile: u32,
    ) -> StoreOutcome {
        if matches!(cmd, viewstate::ViewStateCmd::Reset) {
            self.reconcile.reset();
            self.notify.reset();
            self.notify_map.reset();
        }
        let browse = std::rc::Rc::clone(&self.browse);
        let hubs = &mut self.hubs;
        let person = &mut self.person;
        let collection = &mut self.collection;
        let search = &mut self.search;
        let metadata = &mut self.metadata;
        let watchlist = &mut self.watchlist;
        self.viewstate.borrow_mut().run(
            cmd,
            clients,
            profile,
            &mut |cmd| watchlist_tee(watchlist, cmd, |cmd| browse.borrow_mut().run(cmd, clients)),
            &mut |hubcmd| hubs.run_with_directory(hubcmd, directory, clients),
            &mut |cmd| person.run(cmd),
            &mut |cmd| view_edit_collection(collection, cmd),
            &mut |cmd| search.run_with_directory(cmd, directory),
            &mut |cmd| metadata.run(cmd),
        )
    }

    /// ViewState's route-unconditional landing pass. Fan-out edits and the terminal section-hubs
    /// invalidation are applied to this aggregate's Browse owner before the pump returns. `clients`
    /// and `profile` are the step's inputs, as for [`Stores::viewstate_run`].
    pub fn viewstate_pump(
        &mut self,
        directory: browse::DirectoryView<'_>,
        clients: &plx_plex::plex::ClientKeys,
        profile: u32,
    ) -> EndpointRefreshSet {
        let browse = std::rc::Rc::clone(&self.browse);
        let hubs = &mut self.hubs;
        let person = &mut self.person;
        let collection = &mut self.collection;
        let search = &mut self.search;
        let metadata = &mut self.metadata;
        let watchlist = &mut self.watchlist;
        self.viewstate.borrow_mut().pump_with_gate(
            &self.landgate,
            clients,
            profile,
            &mut |cmd| watchlist_tee(watchlist, cmd, |cmd| browse.borrow_mut().run(cmd, clients)),
            &mut |hubcmd| hubs.run_with_directory(hubcmd, directory, clients),
            &mut |cmd| person.run(cmd),
            &mut |cmd| view_edit_collection(collection, cmd),
            &mut |cmd| search.run_with_directory(cmd, directory),
            &mut |cmd| metadata.run(cmd),
        )
    }

    /// The outboxes the notification map ([`Stores::notify_map`]) can fill, in drain order
    /// (`StoreId` order): a server-side change re-reads its sections' shelves with
    /// `HubsInvalidateServer` and reads the affected source back with `RefetchSourceQuiet`. The
    /// Bridge drains exactly these after the map (`Bridge::start_store_jobs_in`).
    pub const NOTIFY_FILLS: &'static [StoreId] = &[StoreId::Browse, StoreId::Hubs];

    /// The notification feed's per-frame pass. It stands down while the landing gate is armed (a
    /// recording, a replay or a dump owns the run), and never reads the registry's tokens.
    pub fn notify_pump(&mut self) -> notify::NotifyOut {
        self.notify.pump(self.landgate.is_armed())
    }

    /// Hold (`true`) or release the view-state reconcile network work; see [`Stores::network_held`].
    /// Patches are not held: what the stores already show keeps changing while the player is up.
    pub fn hold_network(&mut self, on: bool) {
        self.network_held = on;
        self.viewstate.borrow_mut().set_hold_refresh(on);
    }

    pub fn network_held(&self) -> bool {
        self.network_held
    }

    /// May a re-read of the pages the viewer is not looking at be spent now? Not under the player
    /// ([`Self::network_held`]), and not while a recording or replay is armed on this owner's
    /// landing gate: a re-read is a request no recorded run can contain, so it would desynchronise
    /// the capture from the replay (`plx_machine::landgate`).
    pub fn reread_allowed(&self) -> bool {
        !self.network_held && !self.landgate.is_armed()
    }

    /// Owe the read-back of `keys` (what a page holds on screen, see [`HeldKeys`]) to the
    /// reconcile worker. Never a stop: nothing here scrobbles or feeds an end-of-stream decision.
    /// Quiet: the facts are published but no Detail page is owed a re-read of its own.
    pub fn owe_read_back(&mut self, keys: HeldKeys) {
        self.reconcile.enqueue_reread(keys);
    }

    /// Owe the read-back of `keys` the OWING way: each row that lands also owes the Detail pages
    /// showing it a re-read of their own (`take_landed_views`). The visible Detail's resume.
    pub fn owe_page_read(&mut self, keys: HeldKeys) {
        self.reconcile.enqueue(keys);
    }

        /// What the Library page now on the current section holds (its shelves in the hold window, the
    /// grid rows in the wanted window), or nothing before a listing exists.
    pub fn library_held_keys(&self) -> HeldKeys {
        let mut browse = self.browse.borrow_mut();
        let Some(id) = browse.listing_snapshot().view().id() else { return HeldKeys::new() };
        browse.held_keys(browse::SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section })
    }

    /// Playbacks that stopped. Each gets an immediate resume patch ([`crate::viewstate::ViewFact::Offset`])
    /// on the stores that show the item, so a tile reads the new resume point before any read; and,
    /// when the server took the `stopped` POST, a read-back through the reconcile worker (which
    /// stays queued while the network is held); when it did not, only the Detail pages' own re-read
    /// is owed. The patches' Hubs steps settle refused launches, so the endpoint refreshes they owe
    /// are returned for the caller to carry. MAIN THREAD, route-unconditional.
    pub fn stops_landed(&mut self, stops: Vec<crate::viewstate::StopRecord>, directory: browse::DirectoryView<'_>,
        clients: &plx_plex::plex::ClientKeys) -> EndpointRefreshSet {
        let mut endpoints = EndpointRefreshSet::default();
        for stop in stops {
            endpoints.merge(self.publish(crate::viewstate::ViewChange {
                sid: stop.sid,
                rk: stop.rk.clone(),
                scope: crate::viewstate::Scope::Leaf,
                fact: crate::viewstate::ViewFact::Offset { ms: stop.pos_ms, dur_ms: stop.dur_ms },
            }, directory, clients));
            if stop.ok {
                self.reconcile.enqueue_stop((stop.sid, stop.rk), stop.was_watched);
            } else {
                // No read-back (it would undo the patch), but the Detail pages that show the item
                // were patched too and re-read themselves from the server.
                self.reconcile.owe_page((stop.sid, stop.rk));
            }
        }
        endpoints
    }

    fn publish(&mut self, change: crate::viewstate::ViewChange, directory: browse::DirectoryView<'_>,
        clients: &plx_plex::plex::ClientKeys) -> EndpointRefreshSet {
        self.watchlist.apply_view(&change);
        let browse = std::rc::Rc::clone(&self.browse);
        let hubs = &mut self.hubs;
        let person = &mut self.person;
        let collection = &mut self.collection;
        let search = &mut self.search;
        let metadata = &mut self.metadata;
        crate::viewstate::publish_view(
            change,
            &mut |cmd| browse.borrow_mut().run(cmd, clients),
            &mut |hubcmd| hubs.run_with_directory(hubcmd, directory, clients),
            &mut |cmd| person.run(cmd),
            &mut |cmd| view_edit_collection(collection, cmd),
            &mut |cmd| search.run_with_directory(cmd, directory),
            &mut |cmd| metadata.run(cmd),
        )
    }

    /// The outboxes [`Stores::reconcile_pump`] can fill, in drain order (`StoreId` order): the
    /// end-of-stream fan-out it queues on ViewState's serial queue starts the write at once
    /// ([`StoreJob::ViewWrite`]), and its own step sends the reads ([`StoreJob::ViewReconcile`]).
    /// The Browse requery `Bridge::reconcile_pump` runs after it is a separate step with its own
    /// drain.
    pub const RECONCILE_FILLS: &'static [StoreId] = &[StoreId::ViewState, StoreId::ViewReconcile];

    /// The read-back worker's route-unconditional pass: hand it what landed writes owe, send its
    /// reads, and publish the facts it brought back to the stores that hold the rows. `clients` is
    /// the registry's answer as of this call. It does NOT start the reads it sent: a caller that
    /// returns without taking them ([`Stores::take_jobs_of`]) and handing them to a runner has
    /// stranded the flights. The one production caller is `Bridge::reconcile_pump`. The outcome's
    /// `endpoints` are the refreshes the published facts' Hubs steps settled. `profile` is the one
    /// the end-of-stream fan-outs it queues are made under ([`Stores::viewstate_run`]).
    pub fn reconcile_pump(&mut self, directory: browse::DirectoryView<'_>, clients: &plx_plex::plex::ClientKeys,
        profile: u32) -> StoreOutcome {
        let owed = self.viewstate.borrow_mut().take_reread();
        self.reconcile.enqueue(owed);
        let idle = !self.viewstate.borrow().is_busy() && !self.network_held;
        let browse = std::rc::Rc::clone(&self.browse);
        let hubs = &mut self.hubs;
        let person = &mut self.person;
        let collection = &mut self.collection;
        let search = &mut self.search;
        let metadata = &mut self.metadata;
        let watchlist = &mut self.watchlist;
        let mut endpoints = EndpointRefreshSet::default();
        let changed = self.reconcile.pump_with_gate(&self.landgate, idle, clients, &mut |change| endpoints.merge(crate::viewstate::publish_view(
            { watchlist.apply_view(&change); change },
            &mut |cmd| browse.borrow_mut().run(cmd, clients),
            &mut |hubcmd| hubs.run_with_directory(hubcmd, directory, clients),
            &mut |cmd| person.run(cmd),
            &mut |cmd| view_edit_collection(collection, cmd),
            &mut |cmd| search.run_with_directory(cmd, directory),
            &mut |cmd| metadata.run(cmd),
        )));
        // A first watch whose read-back found the stream's end: the other servers' copies are
        // owed the same watched write, through ViewState's serial queue (the origin already holds it).
        for (sid, rk) in self.reconcile.take_watched_ends() {
            self.viewstate.borrow_mut().fan_out_origin_watched(sid, &rk, clients, profile);
        }
        StoreOutcome { changed, endpoints }
    }

    /// The `(server, ratingKey)` of every row a read-back has landed since the last call: what the
    /// server just said about those items. The application re-reads each Detail page that shows one
    /// (`app::content::reconcile_details`). MAIN THREAD.
    pub fn take_landed_views(&mut self) -> Vec<(plx_plex::plex::ServerId, String)> {
        self.reconcile.take_landed()
    }

    /// Capture all three retained Browse publications from one owner borrow. Directory capture
    /// runs first because resolving profile pins may repoint the current section.
    pub fn capture_browse(&self, directory: &mut browse::DirectorySnapshot)
        -> browse::BrowsePublications {
        let mut browse = self.browse.borrow_mut();
        browse.capture_directory(directory);
        browse::BrowsePublications {
            listing: browse.listing_snapshot(),
            directory: directory.clone(),
            section_hubs: browse.hubs_snapshot(),
        }
    }

    pub fn gen(&self, id: StoreId) -> u32 {
        match id {
            StoreId::Browse => self.browse.borrow().gen(),
            StoreId::Collection => self.collection.gen(),
            StoreId::Hubs => self.hubs.gen(),
            StoreId::Metadata => self.metadata.gen(),
            StoreId::Person => self.person.gen(),
            StoreId::Search => self.search.gen(),
            StoreId::SubtitleSearch => self.subtitle_search.gen(),
            StoreId::ViewState => self.viewstate.borrow().gen(),
            StoreId::ViewReconcile => self.reconcile.gen(),
            StoreId::Watchlist => self.watchlist.gen(),
            StoreId::Catalog => self.catalog.gen(),
            // A landing gate slot only: the stop mailbox is not a store and has no generation.
            StoreId::ViewStop => 0,
        }
    }

    pub fn take_notices(&self) -> Vec<(StoreId, u32)> {
        let mut notices = Vec::new();
        if let Some(generation) = self.browse.borrow().take_notice() {
            notices.push((StoreId::Browse, generation));
        }
        if let Some(generation) = self.hubs.take_notice() {
            notices.push((StoreId::Hubs, generation));
        }
        if let Some(generation) = self.metadata.take_notice() {
            notices.push((StoreId::Metadata, generation));
        }
        if let Some(generation) = self.person.take_notice() {
            notices.push((StoreId::Person, generation));
        }
        if let Some(generation) = self.collection.take_notice() {
            notices.push((StoreId::Collection, generation));
        }
        if let Some(generation) = self.search.take_notice() {
            notices.push((StoreId::Search, generation));
        }
        if let Some(generation) = self.subtitle_search.take_notice() {
            notices.push((StoreId::SubtitleSearch, generation));
        }
        if let Some(generation) = self.viewstate.borrow().take_notice() {
            notices.push((StoreId::ViewState, generation));
        }
        if let Some(generation) = self.reconcile.take_notice() {
            notices.push((StoreId::ViewReconcile, generation));
        }
        if let Some(generation) = self.watchlist.take_notice() {
            notices.push((StoreId::Watchlist, generation));
        }
        if let Some(generation) = self.catalog.take_notice() {
            notices.push((StoreId::Catalog, generation));
        }
        notices
    }
}

/// The application's stores, in the library's ordinal order (`StoreOrd`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StoreId {
    Browse,
    Hubs,
    Metadata,
    Search,
    Person,
    ViewState,
    /// Appended so every pre-collection store keeps its recorded ordinal.
    Collection,
    /// Appended for the same reason: every earlier store keeps its recorded ordinal.
    SubtitleSearch,
    /// Appended for the same reason: the read-back of landed view-state writes.
    ViewReconcile,
    /// Appended for the same reason: the landing gate's slot for the stop mailbox
    /// (`route::take_landed_stops`), which is no store but needs an ordinal of its own because the
    /// gate keeps one mailbox per ordinal.
    ViewStop,
    /// Appended for the same reason: the plex.tv Watchlist (`crate::watchlist`).
    Watchlist,
    /// Appended for the same reason: the catalog page's discover record (`crate::catalog`).
    Catalog,
}

/// Route-scoped background work, distinct from a user command. Polling an idle store must not
/// advance its generation. The dispatcher supplies the frame's time when it delivers the work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreWork {
    Hubs,
    BrowseDiscovery,
    /// Full Library landing pass: pages, menus, discovery and per-section hubs.
    Browse,
    /// Search debounce, worker spawning and result landings. The originating delta survives
    /// the dispatcher's bounded drain carrying this work into a later frame.
    Search { dt_us: u32 },
    /// Subtitle search, download and install poll. Frame-counted, so it carries no delta.
    SubtitleSearch,
}

impl StoreWork {
    pub fn store(self) -> StoreId {
        match self { Self::Hubs => StoreId::Hubs, Self::BrowseDiscovery | Self::Browse => StoreId::Browse,
            Self::Search { .. } => StoreId::Search, Self::SubtitleSearch => StoreId::SubtitleSearch }
    }
}

/// IO a store's step asked the application to start, as a value: the store emits it
/// (`Stores::take_jobs`, over each store's own outbox), the application's runner
/// (`app::adapters::stores`) starts it, and
/// its [`Port`](plx_machine::job::Port) takes the one answer. Unlike [`StoreWork`], which is a timing
/// lane a store is pumped on, a job carries the request itself and the port its answer goes to.
#[derive(Debug, PartialEq)]
pub enum StoreJob {
    /// One detail fetch (`MetadataCmd::RequestDetail`), addressed by its request generation.
    Detail(plx_machine::job::Job<crate::metadata::DetailReq>, crate::metadata::DetailPort),
    /// One collection page request (resolve, header or children), emitted by the Collection pump
    /// and addressed by the listing's generation. One is out at a time.
    Collection(plx_machine::job::Job<crate::collection::CollectionReq>, crate::collection::CollectionPort),
    /// One person fetch (a plex.tv read, or a read of one registered server), emitted by the Person
    /// pump in ascending slot order. Several are out at once (the fan-out), each answering into its
    /// own mailbox slot.
    Person(plx_machine::job::Job<crate::person::PersonReq>, crate::person::PersonPort),
    /// One catalog read: the plex.tv discover record of the title the catalog page is about,
    /// emitted by `open` (and by the pump's retry and its profile-switch re-open) and addressed by
    /// the request id. It names no server and no client: a plex.tv read as the captured identity.
    /// One is out at a time.
    Catalog(plx_machine::job::Job<catalog::CatalogReq>, catalog::CatalogPort),
    /// One subtitle search, download or install poll, emitted by the subtitle-search pump and
    /// addressed by the search generation. One is out at a time. Not on the tape.
    SubtitleSearch(plx_machine::job::Job<crate::subsearch::SubSearchReq>, crate::subsearch::SubSearchPort),
    /// One read-back batch (`/library/metadata/{k1,k2,…}` of one server), emitted by the reconcile
    /// pump and addressed by its flight id. One is out per server. Not on the tape.
    ViewReconcile(plx_machine::job::Job<viewreconcile::ReconcileReq>, viewreconcile::ReconcilePort),
    /// One Home fetch (a source's whole `/hubs`, a row's page, or the Continue Watching deck),
    /// emitted by any Hubs step that kicks a source and addressed by the request id its adapter
    /// minted. Several are out at once, one per source. On the tape only through a controlled run
    /// ([`Admission::Home`]).
    Hubs(plx_machine::job::Job<crate::pms::HubRequest>, crate::pms::HubsPort),
    /// One Library section-hubs read: a section's whole hub list, or one row's window. Emitted by
    /// any Browse step that kicks a section's shelves (a page's `Hubs`, `HubPage`, `HubHold`, an
    /// invalidation, the landing pass) and addressed by the request id the step minted. One is out
    /// across all sections (the section hubs' one claim, `BrowseAdapter::hubs.fetch`). Not on the tape.
    BrowseHubs(plx_machine::job::Job<crate::browse::section_hubs::BrowseHubsReq>,
        crate::browse::section_hubs::BrowseHubsPort),
    /// One of a Library section's value lists: its genres (the Filter menu) or its first letters
    /// (the A-Z rail), emitted by the Browse step that kicks the list and addressed by the request
    /// id the step minted for that list. At most one per list is out (each has its own claim). Not
    /// on the tape.
    BrowseDirectory(plx_machine::job::Job<crate::browse::listing_jobs::BrowseDirectoryReq>,
        crate::browse::listing_jobs::BrowseDirectoryPort),
    /// One page of a Library section's listing, emitted by the Browse pump for the first page the
    /// wanted window is missing and addressed by the request id the step minted. One is out (the
    /// page claim). Not on the tape.
    BrowsePage(plx_machine::job::Job<crate::browse::listing_jobs::BrowsePageReq>,
        crate::browse::listing_jobs::BrowsePagePort),
    /// One search fetch of one source: its first `/hubs/search` answer, or a read of depths of one
    /// result lane, emitted by the Search pump and addressed by the query's generation. Several are
    /// out at once, one per source, each answering into its own mailbox slot (the port names it).
    /// Not on the tape.
    Search(plx_machine::job::Job<crate::search::SearchReq>, crate::search::SearchPort),
    /// One write of the recent Search terms, emitted by the Search step that edited them
    /// (`SearchCmd::RememberRecent` / `ClearRecents`) and addressed by a sequence of its own. It
    /// carries nothing and answers with nothing: a refusal is remembered, and the next edit asks
    /// again. Not on the tape.
    SaveRecents(plx_machine::job::Job<crate::search::recents::SaveRecentsReq>, crate::search::recents::SaveRecentsPort),
    /// One view-state write (a scrobble, an unscrobble or a Remove from Continue Watching) and the
    /// fan-out of a watched toggle to the title's other copies, emitted by the ViewState step
    /// (`request`, the landing pass, or an end-of-stream fan-out) and addressed by the write's id.
    /// One is out at a time: the queue is drained in the order pressed. Not on the tape.
    ViewWrite(plx_machine::job::Job<crate::viewstate::ViewWriteReq>, crate::viewstate::ViewWritePort),
    /// One season switch (`MetadataCmd::LoadSeason`), addressed by the season generation it minted.
    /// The runner resolves the show's server to its client at the launch; a server with none
    /// answers the failure without a GET. Not on the tape.
    MetaSeason(plx_machine::job::Job<crate::metadata::MetaSeasonReq>, crate::metadata::MetaSeasonPort),
    /// One window of a long season's episode pages (`MetadataCmd::WantEpisodes`): at most
    /// `EP_JOB_PAGES` pages, read in order and posted to the port page by page as they are read.
    /// Not on the tape.
    MetaEpisodes(plx_machine::job::Job<crate::metadata::MetaEpisodesReq>, crate::metadata::MetaEpisodesPort),
}

/// How the runner admits a job before it starts it: the policy a job kind belongs to, stated on
/// the kind so a new kind cannot skip the question ([`StoreJob::admission`] is an exhaustive match).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Every launch is recorded on the tape (`stores::tape::admit`) and a replay answers it.
    Tape,
    /// Recorded in a controlled Home run (`HomeIo::admit`, in the order the run asks), and started
    /// with no record anywhere else: the live arms, the landing's follow-up reads and the fan-outs.
    Home,
    /// Never recorded: the launch is the whole decision.
    Direct,
}

impl StoreJob {
    /// The admission family of this job's kind.
    pub fn admission(&self) -> Admission {
        match self {
            Self::Detail(..) | Self::Collection(..) | Self::Person(..) => Admission::Tape,
            Self::Hubs(..) => Admission::Home,
            // A controlled run never emits the section hubs' work: `bootstrap/effects.rs` `store()`
            // accepts only `Browse(Reset)` and `Browse(Discovery(..))` and `work_value()` refuses
            // every `StoreWork` but `Hubs` and `BrowseDiscovery`, so no record exists to place it in
            // and the launch is the whole decision (as for the kinds beside it).
            Self::BrowseHubs(..) => Admission::Direct,
            // A Library directory read (genres, first letters) is asked for by `BrowseCmd::Addressed`
            // with `Genres` or `Letters`, which the controlled-run encoder refuses, so no controlled run or
            // replay emits one and the launch is the whole decision. A controlled run's own Browse
            // commands (Reset, a Discovery landing) emit none: `app/browse_listing_admission_tests.rs`
            // pins it in record and in replay mode.
            Self::BrowseDirectory(..) => Admission::Direct,
            // A listing page is asked for by the Browse pump (`StoreWork::Browse`), which
            // `work_value()` refuses, so no controlled run or replay emits one and the launch is
            // the whole decision. Pinned beside the directory read, in the same file.
            Self::BrowsePage(..) => Admission::Direct,
            Self::Catalog(..)
            | Self::SubtitleSearch(..)
            | Self::ViewReconcile(..)
            | Self::Search(..)
            | Self::SaveRecents(..)
            | Self::ViewWrite(..)
            | Self::MetaSeason(..)
            | Self::MetaEpisodes(..) => Admission::Direct,
        }
    }

    pub fn store(&self) -> StoreId {
        match self {
            Self::Detail(..) | Self::MetaSeason(..) | Self::MetaEpisodes(..) => StoreId::Metadata,
            Self::Collection(..) => StoreId::Collection,
            Self::Person(..) => StoreId::Person,
            Self::Catalog(..) => StoreId::Catalog,
            Self::SubtitleSearch(..) => StoreId::SubtitleSearch,
            Self::ViewReconcile(..) => StoreId::ViewReconcile,
            Self::Hubs(..) => StoreId::Hubs,
            Self::BrowseHubs(..) | Self::BrowseDirectory(..) | Self::BrowsePage(..) => StoreId::Browse,
            Self::Search(..) | Self::SaveRecents(..) => StoreId::Search,
            Self::ViewWrite(..) => StoreId::ViewState,
        }
    }
}

impl StoreId {
    pub const ALL: [StoreId; 12] = [
        StoreId::Browse,
        StoreId::Hubs,
        StoreId::Metadata,
        StoreId::Search,
        StoreId::Person,
        StoreId::ViewState,
        StoreId::Collection,
        StoreId::SubtitleSearch,
        StoreId::ViewReconcile,
        StoreId::ViewStop,
        StoreId::Watchlist,
        StoreId::Catalog,
    ];

    /// The library's ordinal for this store (spec §5.1: the library never names `StoreId`).
    pub fn ord(self) -> StoreOrd {
        StoreOrd(self as u32)
    }

    pub fn from_ord(o: StoreOrd) -> Option<StoreId> {
        StoreId::ALL.get(o.0 as usize).copied()
    }

    pub fn name(self) -> &'static str {
        match self {
            StoreId::Browse => "browse",
            StoreId::Collection => "collection",
            StoreId::Hubs => "hubs",
            StoreId::Metadata => "metadata",
            StoreId::Search => "search",
            StoreId::SubtitleSearch => "subsearch",
            StoreId::Person => "person",
            StoreId::ViewState => "viewstate",
            StoreId::ViewReconcile => "viewreconcile",
            StoreId::ViewStop => "viewstop",
            StoreId::Watchlist => "watchlist",
            StoreId::Catalog => "catalog",
        }
    }
}

/// Every mutation of every store, nested per store so a new `BrowseCmd` moves only Browse's
/// fingerprint (spec §3.1).
#[derive(Clone)]
pub enum StoreCmd {
    Browse(browse::BrowseCmd),
    Hubs(hubs::HubsCmd),
    Metadata(metadata::MetadataCmd),
    Search(search::SearchCmd),
    Person(person::PersonCmd),
    Collection(collection::CollectionCmd),
    ViewState(viewstate::ViewStateCmd),
    SubtitleSearch(subsearch::SubSearchCmd),
}

impl StoreCmd {
    pub fn store(&self) -> StoreId {
        match self {
            StoreCmd::Browse(_) => StoreId::Browse,
            StoreCmd::Hubs(_) => StoreId::Hubs,
            StoreCmd::Metadata(_) => StoreId::Metadata,
            StoreCmd::Search(_) => StoreId::Search,
            StoreCmd::Person(_) => StoreId::Person,
            StoreCmd::Collection(_) => StoreId::Collection,
            StoreCmd::ViewState(_) => StoreId::ViewState,
            StoreCmd::SubtitleSearch(_) => StoreId::SubtitleSearch,
        }
    }
}

/// What a store machine is stepped with: a command, or its once-a-frame landing pass.
#[derive(Clone)]
pub enum StoreEv<C> {
    Cmd(C),
    /// The store's once-a-frame landing pass: land whatever arrived. `dt` is for pumps that
    /// debounce on it. Browse reaches this through `app/bridge.rs`'s `StoreWork` delivery; the
    /// remaining stores retain their legacy pump callers until their ownership slices land.
    #[allow(dead_code)]
    Pump { dt: f32 },
}

// ---------------------------------------------------------------------------------------------
// the landing GATE: a pump's mailbox take, on the frame the recording delivered it (§3.3 step 3)
// ---------------------------------------------------------------------------------------------
//
// Every store here lands OUTSIDE the dispatcher's drain — the legacy pumps poll their own
// mailboxes once a frame — so which frame a worker's answer is observed on was, until phase 11,
// whatever the network and the thread scheduler produced. `plx_machine::landgate` is the schedule; these
// four are the store vocabulary's spelling of it, so a data module wraps its take rather than
// naming the library module and an ordinal by hand. They wrap the TAKE alone and never the pump:
// the retry countdowns, `maybe_spawn` and the debounce must keep running, or the gate would
// suppress the very spawn whose landing it is waiting for.
//
// Off a recording, a replay and a dump each is one relaxed atomic load and the closure's own answer.

/// A one-slot mailbox: `None` while the replay is still waiting for this owner's store's frame.
pub fn take_landing<T>(gate: &plx_machine::landgate::Gate, id: StoreId,
    f: impl FnMut() -> Option<T>) -> Option<T> {
    gate.take(id.ord(), f)
}

/// [`take_landing`] for a site whose request is tracked by `owed` (a [`Fetch::busy`], or the site's own in-flight flag): in the
/// landing gate's DUMP MODE the take delivers the mail the claim owes, so a request that has not
/// been superseded lands on the site's next execution whatever the worker's timing. The blocking
/// dump ([`Gate::arm_dump`](plx_machine::landgate::Gate::arm_dump), for a caller that is not on the
/// frame thread) waits for it, bounded and fail-closed; the PACED dump the frame dump arms
/// ([`Gate::arm_dump_paced`](plx_machine::landgate::Gate::arm_dump_paced)) never waits: a take whose
/// answer is out returns `None` and names the store in `Gate::take_starved`, and the driver holds
/// the frame until it is in. Everywhere else it is exactly [`take_landing`]. `owed` must be
/// the claim of the ONE mailbox `f` takes from, never a store-wide one: a store with several
/// mailboxes would wait on the wrong one.
///
/// For a SUPERSEDABLE request, `f` must be [`Fetch::take_current`], not [`Fetch::take`]: the wait
/// ends on the first mail `f` RETURNS, and `take_current` returns only the answer to the claimed
/// generation, so a superseded request's stale answer neither ends the wait nor releases the new
/// request's claim.
#[track_caller]
pub fn take_landing_owed<T>(gate: &plx_machine::landgate::Gate, id: StoreId,
    owed: impl Fn() -> bool, f: impl FnMut() -> Option<T>) -> Option<T> {
    gate.take_owed(id.ord(), id.name(), owed, f)
}

/// [`take_landings`] for a queue site; see [`take_landing_owed`].
#[track_caller]
pub fn take_landings_owed<T: Send + 'static>(gate: &plx_machine::landgate::Gate, id: StoreId,
    owed: impl Fn(&[T]) -> bool, f: impl FnMut() -> Vec<T>) -> Vec<T> {
    gate.take_all_owed(id.ord(), id.name(), owed, f)
}

/// One fetch's two WORKER-VISIBLE halves: the claim that it is out, and the mailbox its answer
/// lands in — the single-flight mailbox the Person, Search and Collection stores share. Bundled
/// because they move together: the claim of a worker that is out is cleared by the take of the
/// mail answering it ([`Fetch::take`]), so emptying that mailbox any other way owes the release
/// ([`Fetch::clear`]), and a spawn that never happened owes it too ([`Fetch::release`]).
///
/// The claim bounds spawns per *pump*; it is NOT a hard one-worker-at-a-time interlock. A
/// supersede releases it while the old worker is still running, and a take releases it before
/// the owner's generation check (so a stale landing can free a NEWER fetch's claim, costing one
/// duplicate request). Neither can wedge or corrupt: [`Fetch::post`] is monotone on whatever the
/// owner says beats the mail already there, and every owner discards a stale generation.
///
/// That second sentence describes [`Fetch::take`]. The claim also records WHICH GENERATION it was
/// made for ([`Fetch::claim`]), and [`Fetch::take_current`] frees it only for mail that answers
/// that generation: a superseded request's stale answer is dropped and leaves the NEWER request's
/// claim alone, so there is no duplicate request and, in dump mode ([`take_landing_owed`]), the
/// new request's answer lands on the site's next execution whatever order the two workers post in.
pub struct Fetch<M> {
    in_flight: std::sync::atomic::AtomicBool,
    /// The generation the current claim was made for; meaningful only while `in_flight`.
    claimed: std::sync::atomic::AtomicU32,
    /// The generation a REFUSED request was made for and nobody has observed yet
    /// ([`Fetch::refuse_for`], [`Fetch::take_refused`]); [`Fetch::NO_REFUSAL`] when none waits.
    refused: std::sync::atomic::AtomicU64,
    /// Where the worker posts what it came back with. `None` means nothing has landed since the
    /// last take.
    slot: std::sync::Mutex<Option<M>>,
}

impl<M> Default for Fetch<M> {
    fn default() -> Self { Self::IDLE }
}

impl<M> Fetch<M> {
    pub const IDLE: Self = Self {
        in_flight: std::sync::atomic::AtomicBool::new(false),
        claimed: std::sync::atomic::AtomicU32::new(0),
        refused: std::sync::atomic::AtomicU64::new(Self::NO_REFUSAL),
        slot: std::sync::Mutex::new(None),
    };

    /// The `refused` value that names no generation (a generation is a `u32`).
    const NO_REFUSAL: u64 = u64::MAX;

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<M>> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Is the claim held? A spawn's first gate.
    pub fn busy(&self) -> bool {
        self.in_flight.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Is a worker out, or an answer waiting to be taken? What a pump that visits only the
    /// fetches that can have news asks of the ones it has no other reason to look at.
    pub fn outstanding(&self) -> bool {
        self.busy() || self.lock().is_some()
    }

    /// Claim the fetch for generation `gen`, on the way into a spawn. `gen` is the owner's
    /// current generation, the one the worker will stamp its mail with.
    pub fn claim(&self, gen: u32) {
        self.claimed.store(gen, std::sync::atomic::Ordering::SeqCst);
        self.in_flight.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Release the claim without touching the mailbox — what a REFUSED spawn (`task.rs`'s thread
    /// ceiling) owes, since nothing will ever land to release it.
    pub fn release(&self) {
        self.in_flight.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// [`Fetch::release`] for the claim made for `gen`, and no other: a refusal answering a job
    /// whose launch was refused hands back the claim THAT job's step made. Called in the same turn
    /// as the step it answers, the claim is still that job's and this is the plain release; a job
    /// answered later (one a runner holds, or refuses after a newer step has claimed again) finds
    /// the claim is a newer request's and leaves it alone.
    pub fn release_for(&self, gen: u32) {
        if self.holds(gen) {
            self.release();
        }
    }

    /// Is the claim held, and was it made for `gen`? What [`Fetch::release_for`] and
    /// [`Fetch::refuse_for`] hand back, and what neither touches otherwise.
    fn holds(&self, gen: u32) -> bool {
        self.busy() && self.claimed.load(std::sync::atomic::Ordering::SeqCst) == gen
    }

    /// [`Fetch::release_for`] that the owner's step can SEE: the claim made for `gen` is handed
    /// back and the refusal is remembered, for [`Fetch::take_refused`] to answer once.
    ///
    /// A bare release frees the claim and says nothing, so a step that backs off after a launch
    /// that did not happen would find the claim free on its next pump and ask again at once, a
    /// spin. The step reads the refusal at the head of that pump, before it decides to spawn, and
    /// backs off. The claim is still freed in the same call as the refusal, so a take that is
    /// owed on `busy` (dump mode) is never left waiting on a request that will not answer.
    ///
    /// Remembered only when the claim handed back was this request's. A refusal of a request the
    /// owner has since superseded (a parked job refused after a newer one claimed, or after a
    /// [`Fetch::clear`]) names a generation the owner no longer asks about: it releases nothing
    /// and records nothing, so it cannot overwrite the unread refusal of the request that is
    /// current. The refusal is written before the claim is freed, so a reader that finds the claim
    /// free also finds the refusal.
    pub fn refuse_for(&self, gen: u32) {
        if self.holds(gen) {
            self.refused.store(u64::from(gen), std::sync::atomic::Ordering::SeqCst);
            self.release();
        }
    }

    /// Has the request made for `gen` been refused since the last answer to this? True once: the
    /// refusal is consumed. A refusal of any other generation is left where it is, and a
    /// supersede ([`Fetch::clear`]) discards it, since what the retired request was refused no
    /// longer matters.
    pub fn take_refused(&self, gen: u32) -> bool {
        self.refused.compare_exchange(u64::from(gen), Self::NO_REFUSAL,
            std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst).is_ok()
    }

    /// Take whatever landed, RELEASING the claim with it, whatever the mail turns out to be. An
    /// EMPTY mailbox releases nothing: the claim it would clear belongs to a worker still running,
    /// and the next frame would spawn a duplicate.
    ///
    /// A generation-stamped owner uses [`Fetch::take_current`] instead: "whatever the mail turns
    /// out to be" includes the stale answer of a superseded request, which would release a
    /// NEWER request's claim.
    pub fn take(&self) -> Option<M> {
        let mail = self.lock().take()?;
        self.release();
        Some(mail)
    }

    /// Take the mail that answers the CLAIMED generation, releasing the claim with it. `gen_of`
    /// reads the generation a piece of mail was stamped with.
    ///
    /// While a claim is held, mail stamped with any OTHER generation is a superseded request's
    /// late answer: it is dropped here (the owner would discard it on its generation check
    /// anyway) and the claim stays held for the request that is still out. With no claim held
    /// there is nothing to protect, and mail is taken as [`Fetch::take`] would.
    pub fn take_current(&self, gen_of: impl Fn(&M) -> u32) -> Option<M> {
        let mut slot = self.lock();
        let mail = slot.take()?;
        if self.busy() && gen_of(&mail) != self.claimed.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        drop(slot);
        self.release();
        Some(mail)
    }

    /// Drop the mailbox and release the claim with it — a supersede's per-fetch half. Releases
    /// unconditionally, unlike [`Fetch::take`]: the worker holding this claim is still out, and
    /// what it will answer about is no longer open. A refusal nobody has observed yet
    /// ([`Fetch::refuse_for`]) is discarded with it, for the same reason.
    pub fn clear(&self) {
        *self.lock() = None;
        self.release();
        self.refused.store(Self::NO_REFUSAL, std::sync::atomic::Ordering::SeqCst);
    }

    /// WORKER THREAD: post `mail` unless the mail already waiting wins — `beats(old)` answers
    /// whether the new mail replaces it. MONOTONE by the owner's rule: an older fetch landing late
    /// must never clobber a newer result the pump has not consumed yet.
    pub fn post(&self, mail: M, beats: impl FnOnce(&M) -> bool) {
        let mut slot = self.lock();
        if slot.as_ref().is_none_or(beats) {
            *slot = Some(mail);
        }
    }

    /// Is mail waiting? A pump's question before it takes, and a test's view of the mailbox without
    /// taking it.
    pub fn has_mail(&self) -> bool {
        self.lock().is_some()
    }
}

/// A mailbox drained as a QUEUE: an empty answer is not a landing.
pub fn take_landings<T>(gate: &plx_machine::landgate::Gate, id: StoreId,
    f: impl FnMut() -> Vec<T>) -> Vec<T> {
    gate.take_all(id.ord(), f)
}


#[cfg(test)]
mod tests {
    #[test]
    fn data_layers_do_not_execute_auth_endpoint_recovery() {
        for file in ["pms.rs", "browse/mod.rs", "viewstate.rs"] {
            let source = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file),
            ).unwrap();
            assert!(!source.contains("auth::request_endpoint_refresh("),
                "{file} still executes endpoint recovery instead of returning a neutral outcome");
        }
    }
    use super::*;

    // `a_command_raises_one_notice_and_a_steady_store_none` (the global `apply(StoreCmd::Metadata
    // (Clear))` regression) and `generic_dispatch_rejects_all_physically_owned_stores` (which
    // proved the same global `apply` panicked for the already-owned stores) are deleted: the
    // free `apply`/`take_notices`/`gen` dispatcher they tested no longer exists anywhere in
    // production code. Every store — Metadata included, as of this layer — now dispatches
    // through its own per-owner `run`/`step`, so there is no shared router left to grade.

    fn dump_gate(timeout_ms: u64) -> plx_machine::landgate::Gate {
        let gate = plx_machine::landgate::Gate::default();
        gate.arm_dump(std::time::Duration::from_millis(timeout_ms));
        gate
    }

    /// Dump mode on the REAL single-flight mailbox: a request spawned in one iteration (and not
    /// superseded since) lands on the site's next execution however late the worker posts, and
    /// the take releases the claim. Primitive only: each converted site has its own pump test.
    #[test]
    fn a_fetch_spawned_in_one_iteration_lands_on_the_next_whatever_the_worker() {
        for delay_us in [0u64, 200, 2_000, 9_000] {
            let gate = dump_gate(20_000);
            let fetch = std::sync::Arc::new(Fetch::<u32>::IDLE);
            assert!(take_landing_owed(&gate, StoreId::Person, || fetch.busy(), || fetch.take_current(|m| *m)).is_none());
            fetch.claim(7);
            let worker = std::sync::Arc::clone(&fetch);
            let join = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_micros(delay_us));
                worker.post(7, |_| true);
            });
            let got = take_landing_owed(&gate, StoreId::Person, || fetch.busy(), || fetch.take_current(|m| *m));
            assert_eq!(got, Some(7), "delay {delay_us}us");
            assert!(!fetch.busy(), "the TAKE released the claim");
            join.join().unwrap();
        }
    }

    /// What this pins, and only this: `clear` (a supersede) releases the claim, so a dump take
    /// returns at once instead of waiting for an answer nobody owes (an `owed` that was ignored,
    /// or a claim that survived the clear, would fail closed here). The stale answer racing a
    /// NEWER claim is `a_stale_answer_leaves_the_new_claim_held` below.
    #[test]
    fn a_cleared_claim_causes_no_dump_wait() {
        let gate = dump_gate(100); // any wait on the cleared claim would fail closed (panic) here
        let fetch = Fetch::<u32>::IDLE;
        fetch.claim(1);
        fetch.clear();
        assert_eq!(take_landing_owed(&gate, StoreId::Person, || fetch.busy(), || fetch.take_current(|m| *m)), None);
    }

    /// The sequence of the review's race model, on the real mailbox: claim(1), supersede, claim(2),
    /// the generation-1 worker posts late. `take_current` drops that answer and leaves claim 2
    /// held; only generation 2's own answer releases it. (`take` would have released claim 2 on
    /// the stale answer, and the next pump would have spawned a duplicate request.)
    #[test]
    fn a_stale_answer_leaves_the_new_claim_held() {
        let fetch = Fetch::<u32>::IDLE;
        fetch.claim(1);
        fetch.clear();
        fetch.claim(2);
        fetch.post(1, |_| true); // request 1's worker, late
        assert_eq!(fetch.take_current(|m| *m), None, "the stale answer is dropped, not returned");
        assert!(fetch.busy(), "...and request 2's claim survives it");
        assert!(!fetch.has_mail(), "the stale mail is gone");
        fetch.post(2, |_| true);
        assert_eq!(fetch.take_current(|m| *m), Some(2));
        assert!(!fetch.busy(), "only the answer to generation 2 releases its claim");
    }

    /// A refusal hands back the claim of the request it answers, and only that one: a job refused
    /// after its owner has superseded it and claimed again must not free the newer request's claim
    /// (the next pump would spawn a duplicate).
    #[test]
    fn a_refusal_releases_only_its_own_generations_claim() {
        let fetch = Fetch::<u32>::IDLE;
        fetch.claim(1);
        fetch.release_for(1);
        assert!(!fetch.busy(), "the refused request's own claim is handed back");

        fetch.claim(1);
        fetch.clear();
        fetch.claim(2);
        fetch.release_for(1);
        assert!(fetch.busy(), "request 1's refusal leaves request 2's claim held");
        fetch.release_for(2);
        assert!(!fetch.busy());
        fetch.release_for(2);
        assert!(!fetch.busy(), "releasing a released claim is a no-op");
    }

    /// A refusal is a release the owner's step can see: it frees the claim, and the step's next
    /// look at that generation answers `true` ONCE. Without the memory the step would find the
    /// claim free and ask again at once.
    #[test]
    fn a_refusal_frees_the_claim_and_is_observed_once() {
        let fetch = Fetch::<u32>::IDLE;
        assert!(!fetch.take_refused(1), "nothing was refused");
        fetch.claim(1);
        fetch.refuse_for(1);
        assert!(!fetch.busy(), "the refused request's claim is handed back in the same call");
        assert!(fetch.take_refused(1), "the owner's next pump sees the refusal");
        assert!(!fetch.take_refused(1), "...once");

        // generation 0 is a generation like any other
        fetch.claim(0);
        fetch.refuse_for(0);
        assert!(fetch.take_refused(0));
    }

    /// The refusal belongs to the generation it answers: another generation's step does not
    /// take it, and a refusal of a request the owner has since superseded does not free the newer
    /// request's claim.
    #[test]
    fn a_refusal_is_for_its_own_generation_only() {
        let fetch = Fetch::<u32>::IDLE;
        fetch.claim(1);
        fetch.refuse_for(1);
        assert!(!fetch.take_refused(2), "generation 2 was not refused");
        assert!(fetch.take_refused(1), "...and asking about it did not consume generation 1's");

        fetch.claim(1);
        fetch.clear();
        fetch.claim(2);
        fetch.refuse_for(1); // request 1's launch is refused after request 2 has claimed
        assert!(fetch.busy(), "request 2's claim is not request 1's to hand back");
        assert!(!fetch.take_refused(2));
        assert!(!fetch.take_refused(1), "...and a superseded request's refusal is not remembered");
    }

    /// The order that overwrote: the CURRENT request's refusal is unread when a superseded
    /// request's refusal arrives (a `Held` runner refuses a parked job after the newer one). The
    /// late one must leave the unread one to be taken, or the owner finds no refusal and asks
    /// again at once.
    #[test]
    fn a_superseded_refusal_does_not_overwrite_the_current_ones() {
        let fetch = Fetch::<u32>::IDLE;
        fetch.claim(1);
        fetch.clear(); // request 1 is superseded while its job is parked
        fetch.claim(2);
        fetch.refuse_for(2); // request 2 is refused at its start
        fetch.refuse_for(1); // then the parked request 1 is refused
        assert!(!fetch.busy());
        assert!(fetch.take_refused(2), "request 2's refusal is still there for its owner");
        assert!(!fetch.take_refused(1), "request 1's was never recorded");

        // after a clear, with nothing claimed, a stale refusal records nothing either
        fetch.claim(3);
        fetch.clear();
        fetch.refuse_for(3);
        assert!(!fetch.take_refused(3));
    }

    /// A supersede retires whatever the old request was going to say, a refusal included: it is not
    /// left to be taken by a later request that happens to carry the same generation.
    #[test]
    fn a_supersede_discards_an_unobserved_refusal() {
        let fetch = Fetch::<u32>::IDLE;
        fetch.claim(1);
        fetch.refuse_for(1);
        fetch.clear();
        assert!(!fetch.take_refused(1));
    }

    /// With NO claim held there is nothing to protect: mail is taken as before and the owner's
    /// generation check discards it (the tests that land mail without a spawn rely on this).
    #[test]
    fn mail_with_no_claim_held_is_taken_as_before() {
        let fetch = Fetch::<u32>::IDLE;
        fetch.post(7, |_| true);
        assert_eq!(fetch.take_current(|m| *m), Some(7));
        assert!(!fetch.busy());
    }

    /// The same sequence through the dump wait: the stale answer lands first, B's lands later, and
    /// the take that was owed B's answer returns B's, whatever the delays.
    #[test]
    fn a_stale_answer_does_not_end_the_dump_wait() {
        for stale_us in [0u64, 300, 3_000] {
            let gate = dump_gate(20_000);
            let fetch = std::sync::Arc::new(Fetch::<u32>::IDLE);
            fetch.claim(1);
            fetch.clear();
            fetch.claim(2);
            let worker = std::sync::Arc::clone(&fetch);
            let join = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_micros(stale_us));
                worker.post(1, |_| true);
                std::thread::sleep(std::time::Duration::from_millis(6));
                worker.post(2, |_| true);
            });
            let got = take_landing_owed(&gate, StoreId::Person, || fetch.busy(), || fetch.take_current(|m| *m));
            assert_eq!(got, Some(2), "stale after {stale_us}us");
            assert!(!fetch.busy());
            join.join().unwrap();
        }
    }

    #[test]
    fn the_ordinal_round_trips_for_every_store() {
        for id in StoreId::ALL {
            assert_eq!(StoreId::from_ord(id.ord()), Some(id));
        }
        assert_eq!(StoreId::from_ord(StoreOrd(12)), None);
    }

    /// A fan-out call that can fill an outbox names it in a `*_FILLS` list the Bridge drains. A
    /// new emitter joins the list of every call that reaches it (Browse appends itself where its
    /// calls do), so the content is pinned: an emitter left off a list strands its jobs until the
    /// backstop, which the test rigs count as a bypass.
    #[test]
    fn the_fan_out_fill_lists_name_the_stores_the_calls_can_fill() {
        assert_eq!(Stores::VIEWSTATE_FILLS, [StoreId::Browse, StoreId::Hubs, StoreId::ViewState]);
        assert_eq!(Stores::NOTIFY_FILLS, [StoreId::Browse, StoreId::Hubs]);
        assert_eq!(Stores::RECONCILE_FILLS, [StoreId::ViewState, StoreId::ViewReconcile]);
        for list in [Stores::VIEWSTATE_FILLS, Stores::NOTIFY_FILLS, Stores::RECONCILE_FILLS] {
            for (i, id) in list.iter().enumerate() {
                assert!(StoreId::ALL.contains(id), "{id:?} is a store");
                assert!(!list[..i].contains(id), "{id:?} is listed once");
                assert!(i == 0 || (list[i - 1] as u32) < (*id as u32), "{id:?} is listed in StoreId order");
            }
        }
    }
}

#[cfg(test)]
#[path = "notify_map_tests.rs"]
mod notify_map_tests;

#[cfg(test)]
#[path = "held_keys_tests.rs"]
mod held_keys_tests;

/// ViewState publishes a press (and each fan-out copy) through `publish_view`, whose first
/// consumer is Browse's `ApplyView`. The Watchlist is patched from that same command, so the user's
/// own watched/unwatched press reaches a matched tile at the press frame like every other surface
/// rather than waiting for the read-back.
fn watchlist_tee(
    watchlist: &mut watchlist::WatchlistStore,
    cmd: browse::BrowseCmd,
    run: impl FnOnce(browse::BrowseCmd) -> bool,
) -> bool {
    if let browse::BrowseCmd::ApplyView { at, edit } = &cmd {
        watchlist.apply_edit(at.sid, &at.rk, *edit, None);
    }
    run(cmd)
}

/// ViewState's route into the Collection store. The only command ViewState sends it is
/// `ApplyView` (an optimistic edit of a member), which neither reads a client nor emits a job, so
/// it runs with no keys. Any other command would read "no server holds a client" and must come
/// through `Stores::collection_run` with the host's keys instead.
fn view_edit_collection(collection: &mut collection::CollectionStore, cmd: collection::CollectionCmd) -> bool {
    debug_assert!(matches!(cmd, collection::CollectionCmd::ApplyView { .. }),
        "ViewState sends the Collection store only ApplyView; anything else needs the host's client keys");
    collection.run(cmd, &plx_plex::plex::ClientKeys::default())
}
