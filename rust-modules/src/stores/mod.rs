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
//! crates, `ui::machine` and — since
//! phase 11's landing schedule — `ui::landgate`, and nothing else (spec §2.1's layer rule;
//! `ci/check-deps.sh`'s `mutators` gate refuses the old spelling outside `stores/` and the data
//! modules).

use crate::ui::machine::StoreOrd;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EndpointRefresh { pub sid: crate::plex::ServerId }

/// Advisory requests in first-observation order. Invalid IDs are rejected, never remapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub(crate) struct EndpointRefreshSet {
    ids: [crate::plex::ServerId; crate::plex::MAX_SERVERS],
    len: usize,
}

impl Default for EndpointRefreshSet {
    fn default() -> Self { Self { ids: [crate::plex::ServerId::UNSET; crate::plex::MAX_SERVERS], len: 0 } }
}

impl EndpointRefreshSet {
    pub(crate) fn insert(&mut self, request: EndpointRefresh) -> bool {
        if request.sid.raw() as usize >= crate::plex::MAX_SERVERS
            || self.ids[..self.len].contains(&request.sid) { return false; }
        self.ids[self.len] = request.sid;
        self.len += 1;
        true
    }
    pub(crate) fn merge(&mut self, other: Self) {
        for request in other.iter() { self.insert(request); }
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = EndpointRefresh> + '_ {
        self.ids[..self.len].iter().map(|&sid| EndpointRefresh { sid })
    }
    pub(crate) fn emit<H: StoreEffectHost>(self, fx: &mut crate::ui::machine::Effects<'_, H>) {
        for request in self.iter() { fx.push(crate::ui::machine::Fx::App(H::endpoint_refresh(request))); }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[must_use]
pub(crate) struct StoreOutcome {
    pub changed: bool,
    pub endpoints: EndpointRefreshSet,
}

impl StoreOutcome {
    pub(crate) fn changed(changed: bool) -> Self { Self { changed, ..Self::default() } }
}

pub(crate) trait StoreEffectHost: crate::ui::machine::Host {
    fn endpoint_refresh(request: EndpointRefresh) -> Self::Fx;
}

pub(crate) mod browse;
mod content_arg;
pub(crate) use content_arg::ContentArg;
pub(crate) mod hubs;
pub(crate) mod metadata;
pub(crate) mod person;
pub(crate) mod collection;
pub(crate) mod search;
pub(crate) mod tape;
pub(crate) mod viewstate;

/// Production store aggregate. All seven stores are physical owners here — Browse, Person and
/// ViewState landed first; Search, Hubs and Metadata completed the port.
pub(crate) struct Stores {
    /// The landing schedule belongs to the same application owner as these stores. Two Bridges
    /// may contain the same `StoreId`; they must not share its replay cursor or wait budget.
    pub(crate) landgate: crate::ui::landgate::Gate,
    pub(crate) browse: std::rc::Rc<std::cell::RefCell<browse::BrowseStore>>,
    pub(crate) hubs: hubs::HubsStore,
    pub(crate) metadata: metadata::MetadataStore,
    pub(crate) person: person::PersonStore,
    pub(crate) collection: collection::CollectionStore,
    pub(crate) search: search::SearchStore,
    pub(crate) viewstate: std::cell::RefCell<viewstate::ViewStateStore>,
}

impl Default for Stores {
    fn default() -> Self {
        let browse = std::rc::Rc::new(std::cell::RefCell::new(browse::BrowseStore::default()));
        Self {
            landgate: Default::default(),
            browse,
            hubs: hubs::HubsStore::default(),
            metadata: metadata::MetadataStore::default(),
            person: person::PersonStore::default(),
            collection: collection::CollectionStore::default(),
            search: search::SearchStore::default(),
            viewstate: std::cell::RefCell::new(viewstate::ViewStateStore::default()),
        }
    }
}

impl Stores {
    /// Explicit synchronous Browse command path. The answer is available before this call returns.
    pub(crate) fn browse_run(&self, cmd: browse::BrowseCmd) -> bool {
        self.browse.borrow_mut().run(cmd)
    }

    pub(crate) fn browse_discover_pump(&self) -> StoreOutcome {
        self.browse.borrow_mut().discover_pump_with_gate(&self.landgate)
    }

    pub(crate) fn person_run(&mut self, cmd: person::PersonCmd) -> bool {
        self.person.run(cmd)
    }

    pub(crate) fn collection_run(&mut self, cmd: collection::CollectionCmd) -> bool {
        self.collection.run(cmd)
    }

    pub(crate) fn collection_pump(&mut self) -> bool { self.collection.pump(&self.landgate) }

    pub(crate) fn collection_view(&self) -> crate::collection::CollectionView<'_> {
        self.collection.view()
    }

    pub(crate) fn person_pump(&mut self) -> bool {
        self.person.pump(&self.landgate)
    }

    pub(crate) fn person_view(&self) -> crate::person::PersonView<'_> {
        self.person.view()
    }

    pub(crate) fn metadata_run(&mut self, cmd: metadata::MetadataCmd) -> bool {
        self.metadata.run(cmd)
    }

    pub(crate) fn metadata_pump(&mut self) -> bool {
        self.metadata.pump(&self.landgate)
    }

    pub(crate) fn metadata_view(&self) -> crate::metadata::MetadataView<'_> {
        self.metadata.view()
    }

    pub(crate) fn search_run(
        &mut self,
        cmd: search::SearchCmd,
        directory: browse::DirectoryView<'_>,
    ) -> bool {
        self.search.run_with_directory(cmd, directory)
    }

    pub(crate) fn search_pump(&mut self, dt: f32, directory: browse::DirectoryView<'_>) -> bool {
        self.search.pump_with_directory_and_gate(dt, directory, &self.landgate)
    }

    pub(crate) fn search_snapshot(&self, directory: browse::DirectoryView<'_>) -> search::SearchSnapshot {
        self.search.snapshot_with_directory(directory)
    }

    /// Controlled discovery against this aggregate's Browse owner.
    pub(crate) fn browse_controlled_discover(
        &self,
        launch: &mut dyn FnMut(crate::browse::DiscoveryRequest) -> bool,
    ) {
        self.browse.borrow_mut().controlled_discover(launch);
    }

    /// ViewState's synchronous command path, with every Browse side effect addressed back to this
    /// aggregate. The callback is invoked inline, preserving the press-frame optimistic edit.
    pub(crate) fn viewstate_run(
        &mut self,
        cmd: viewstate::ViewStateCmd,
        directory: browse::DirectoryView<'_>,
    ) -> bool {
        let browse = std::rc::Rc::clone(&self.browse);
        let hubs = &mut self.hubs;
        let person = &mut self.person;
        let collection = &mut self.collection;
        let search = &mut self.search;
        let metadata = &mut self.metadata;
        self.viewstate.borrow_mut().run(
            cmd,
            &mut |cmd| browse.borrow_mut().run(cmd),
            &mut |hubcmd| hubs.run_with_directory(hubcmd, directory),
            &mut |cmd| person.run(cmd),
            &mut |cmd| collection.run(cmd),
            &mut |cmd| search.run_with_directory(cmd, directory),
            &mut |cmd| metadata.run(cmd),
        )
    }

    /// ViewState's route-unconditional landing pass. Fan-out edits and the terminal section-hubs
    /// invalidation are applied to this aggregate's Browse owner before the pump returns.
    pub(crate) fn viewstate_pump(
        &mut self,
        directory: browse::DirectoryView<'_>,
    ) -> EndpointRefreshSet {
        let browse = std::rc::Rc::clone(&self.browse);
        let hubs = &mut self.hubs;
        let person = &mut self.person;
        let collection = &mut self.collection;
        let search = &mut self.search;
        let metadata = &mut self.metadata;
        self.viewstate.borrow_mut().pump_with_gate(
            &self.landgate,
            &mut |cmd| browse.borrow_mut().run(cmd),
            &mut |hubcmd| hubs.run_with_directory(hubcmd, directory),
            &mut |cmd| person.run(cmd),
            &mut |cmd| collection.run(cmd),
            &mut |cmd| search.run_with_directory(cmd, directory),
            &mut |cmd| metadata.run(cmd),
        )
    }

    pub(crate) fn take_detail_refresh(&self) -> Option<viewstate::DetailRefresh> {
        self.viewstate.borrow_mut().take_detail_refresh()
    }

    /// Capture all three retained Browse publications from one owner borrow. Directory capture
    /// runs first because resolving profile pins may repoint the current section.
    pub(crate) fn capture_browse(&self, directory: &mut browse::DirectorySnapshot)
        -> browse::BrowsePublications {
        let mut browse = self.browse.borrow_mut();
        browse.capture_directory(directory);
        browse::BrowsePublications {
            listing: browse.listing_snapshot(),
            directory: directory.clone(),
            section_hubs: browse.hubs_snapshot(),
        }
    }

    pub(crate) fn gen(&self, id: StoreId) -> u32 {
        match id {
            StoreId::Browse => self.browse.borrow().gen(),
            StoreId::Collection => self.collection.gen(),
            StoreId::Hubs => self.hubs.gen(),
            StoreId::Metadata => self.metadata.gen(),
            StoreId::Person => self.person.gen(),
            StoreId::Search => self.search.gen(),
            StoreId::ViewState => self.viewstate.borrow().gen(),
        }
    }

    pub(crate) fn take_notices(&self) -> Vec<(StoreId, u32)> {
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
        if let Some(generation) = self.viewstate.borrow().take_notice() {
            notices.push((StoreId::ViewState, generation));
        }
        notices
    }
}

/// The application's stores, in the library's ordinal order (`StoreOrd`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StoreId {
    Browse,
    Hubs,
    Metadata,
    Search,
    Person,
    ViewState,
    /// Appended so every pre-collection store keeps its recorded ordinal.
    Collection,
}

/// Route-scoped background work, distinct from a user command. Polling an idle store must not
/// advance its generation. The dispatcher supplies the frame's time when it delivers the work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StoreWork {
    Hubs,
    BrowseDiscovery,
    /// Full Library landing pass: pages, menus, discovery and per-section hubs.
    Browse,
    /// Search debounce, worker spawning and result landings. The originating delta survives
    /// the dispatcher's bounded drain carrying this work into a later frame.
    Search { dt_us: u32 },
}

impl StoreWork {
    pub(crate) fn store(self) -> StoreId {
        match self { Self::Hubs => StoreId::Hubs, Self::BrowseDiscovery | Self::Browse => StoreId::Browse,
            Self::Search { .. } => StoreId::Search }
    }
}

impl StoreId {
    pub(crate) const ALL: [StoreId; 7] = [
        StoreId::Browse,
        StoreId::Hubs,
        StoreId::Metadata,
        StoreId::Search,
        StoreId::Person,
        StoreId::ViewState,
        StoreId::Collection,
    ];

    /// The library's ordinal for this store (spec §5.1: the library never names `StoreId`).
    pub(crate) fn ord(self) -> StoreOrd {
        StoreOrd(self as u32)
    }

    pub(crate) fn from_ord(o: StoreOrd) -> Option<StoreId> {
        StoreId::ALL.get(o.0 as usize).copied()
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            StoreId::Browse => "browse",
            StoreId::Collection => "collection",
            StoreId::Hubs => "hubs",
            StoreId::Metadata => "metadata",
            StoreId::Search => "search",
            StoreId::Person => "person",
            StoreId::ViewState => "viewstate",
        }
    }
}

/// Every mutation of every store, nested per store so a new `BrowseCmd` moves only Browse's
/// fingerprint (spec §3.1).
#[derive(Clone)]
pub(crate) enum StoreCmd {
    Browse(browse::BrowseCmd),
    Hubs(hubs::HubsCmd),
    Metadata(metadata::MetadataCmd),
    Search(search::SearchCmd),
    Person(person::PersonCmd),
    Collection(collection::CollectionCmd),
    ViewState(viewstate::ViewStateCmd),
}

impl StoreCmd {
    pub(crate) fn store(&self) -> StoreId {
        match self {
            StoreCmd::Browse(_) => StoreId::Browse,
            StoreCmd::Hubs(_) => StoreId::Hubs,
            StoreCmd::Metadata(_) => StoreId::Metadata,
            StoreCmd::Search(_) => StoreId::Search,
            StoreCmd::Person(_) => StoreId::Person,
            StoreCmd::Collection(_) => StoreId::Collection,
            StoreCmd::ViewState(_) => StoreId::ViewState,
        }
    }
}

/// What a store machine is stepped with: a command, or its once-a-frame landing pass.
#[derive(Clone)]
pub(crate) enum StoreEv<C> {
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
// whatever the network and the thread scheduler produced. `ui::landgate` is the schedule; these
// two are the store vocabulary's spelling of it, so a data module wraps its take rather than
// naming the library module and an ordinal by hand. They wrap the TAKE alone and never the pump:
// the retry countdowns, `maybe_spawn` and the debounce must keep running, or the gate would
// suppress the very spawn whose landing it is waiting for.
//
// Off a recording and off a replay each is one relaxed atomic load and the closure's own answer.

/// A one-slot mailbox: `None` while the replay is still waiting for this owner's store's frame.
pub(crate) fn take_landing<T>(gate: &crate::ui::landgate::Gate, id: StoreId,
    f: impl FnMut() -> Option<T>) -> Option<T> {
    gate.take(id.ord(), f)
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
pub(crate) struct Fetch<M> {
    in_flight: std::sync::atomic::AtomicBool,
    /// Where the worker posts what it came back with. `None` means nothing has landed since the
    /// last take.
    slot: std::sync::Mutex<Option<M>>,
}

impl<M> Default for Fetch<M> {
    fn default() -> Self { Self::IDLE }
}

impl<M> Fetch<M> {
    pub(crate) const IDLE: Self = Self {
        in_flight: std::sync::atomic::AtomicBool::new(false),
        slot: std::sync::Mutex::new(None),
    };

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<M>> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Is the claim held? A spawn's first gate.
    pub(crate) fn busy(&self) -> bool {
        self.in_flight.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Claim the fetch, on the way into a spawn.
    pub(crate) fn claim(&self) {
        self.in_flight.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Release the claim without touching the mailbox — what a REFUSED spawn (`task.rs`'s thread
    /// ceiling) owes, since nothing will ever land to release it.
    pub(crate) fn release(&self) {
        self.in_flight.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Take whatever landed, RELEASING the claim with it, whatever the mail turns out to be. An
    /// EMPTY mailbox releases nothing: the claim it would clear belongs to a worker still running,
    /// and the next frame would spawn a duplicate.
    pub(crate) fn take(&self) -> Option<M> {
        let mail = self.lock().take()?;
        self.release();
        Some(mail)
    }

    /// Drop the mailbox and release the claim with it — a supersede's per-fetch half. Releases
    /// unconditionally, unlike [`Fetch::take`]: the worker holding this claim is still out, and
    /// what it will answer about is no longer open.
    pub(crate) fn clear(&self) {
        *self.lock() = None;
        self.release();
    }

    /// WORKER THREAD: post `mail` unless the mail already waiting wins — `beats(old)` answers
    /// whether the new mail replaces it. MONOTONE by the owner's rule: an older fetch landing late
    /// must never clobber a newer result the pump has not consumed yet.
    pub(crate) fn post(&self, mail: M, beats: impl FnOnce(&M) -> bool) {
        let mut slot = self.lock();
        if slot.as_ref().is_none_or(beats) {
            *slot = Some(mail);
        }
    }

    /// Is mail waiting? A test's view of the mailbox without taking it.
    #[cfg(test)]
    pub(crate) fn has_mail(&self) -> bool {
        self.lock().is_some()
    }
}

/// A mailbox drained as a QUEUE: an empty answer is not a landing.
pub(crate) fn take_landings<T>(gate: &crate::ui::landgate::Gate, id: StoreId,
    f: impl FnMut() -> Vec<T>) -> Vec<T> {
    gate.take_all(id.ord(), f)
}


#[cfg(test)]
mod tests {
    #[test]
    fn endpoint_sets_preserve_first_observation_order_and_capacity() {
        let request = |id| EndpointRefresh { sid: crate::plex::ServerId::from_raw(id) };
        let mut first = EndpointRefreshSet::default();
        assert_eq!(first.iter().count(), 0);
        assert!(!first.insert(request(u16::MAX)));
        assert!(!first.insert(request(crate::plex::MAX_SERVERS as u16)));
        first.insert(request(3)); first.insert(request(1)); first.insert(request(3));
        let mut second = EndpointRefreshSet::default();
        second.insert(request(1)); second.insert(request(2)); second.insert(request(0));
        first.merge(second);
        assert_eq!(first.iter().map(|r| r.sid.raw()).collect::<Vec<_>>(), [3, 1, 2, 0]);
        for id in 0..crate::plex::MAX_SERVERS { first.insert(request(id as u16)); }
        assert_eq!(first.iter().count(), crate::plex::MAX_SERVERS);
        assert!(first.iter().count() <= crate::ui::machine::MAX_EMIT_PER_STEP as usize);
        first.merge(first);
        assert_eq!(first.iter().count(), crate::plex::MAX_SERVERS);
    }
    #[test]
    fn data_layers_do_not_execute_auth_endpoint_recovery() {
        for file in ["pms.rs", "browse/mod.rs", "viewstate.rs"] {
            let source = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file),
            ).unwrap();
            assert!(!source.contains("crate::auth::request_endpoint_refresh("),
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

    #[test]
    fn the_ordinal_round_trips_for_every_store() {
        for id in StoreId::ALL {
            assert_eq!(StoreId::from_ord(id.ord()), Some(id));
        }
        assert_eq!(StoreId::from_ord(StoreOrd(7)), None);
    }
}
