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

pub mod browse;
mod content_arg;
pub use content_arg::ContentArg;
pub mod hubs;
pub mod metadata;
pub mod person;
pub mod collection;
pub mod page_cache;
pub mod search;
pub mod subsearch;
pub mod tape;
pub mod viewstate;

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
            subtitle_search: subsearch::SubSearchStore::default(),
            viewstate: std::cell::RefCell::new(viewstate::ViewStateStore::default()),
        }
    }
}

impl Stores {
    /// Explicit synchronous Browse command path. The answer is available before this call returns.
    pub fn browse_run(&self, cmd: browse::BrowseCmd) -> bool {
        self.browse.borrow_mut().run(cmd)
    }

    pub fn browse_discover_pump(&self) -> StoreOutcome {
        self.browse.borrow_mut().discover_pump_with_gate(&self.landgate)
    }

    pub fn person_run(&mut self, cmd: person::PersonCmd) -> bool {
        self.person.run(cmd)
    }

    pub fn collection_run(&mut self, cmd: collection::CollectionCmd) -> bool {
        self.collection.run(cmd)
    }

    pub fn collection_pump(&mut self) -> bool { self.collection.pump(&self.landgate) }

    pub fn collection_view(&self) -> crate::collection::CollectionView<'_> {
        self.collection.view()
    }

    pub fn subtitle_search_run(&mut self, cmd: subsearch::SubSearchCmd) -> bool {
        self.subtitle_search.run(cmd)
    }

    pub fn subtitle_search_pump(&mut self) -> bool { self.subtitle_search.pump(&self.landgate) }

    pub fn subtitle_search_view(&self) -> crate::subsearch::SubSearchView<'_> {
        self.subtitle_search.view()
    }

    pub fn person_pump(&mut self) -> bool {
        self.person.pump(&self.landgate)
    }

    pub fn person_view(&self) -> crate::person::PersonView<'_> {
        self.person.view()
    }

    pub fn metadata_run(&mut self, cmd: metadata::MetadataCmd) -> bool {
        self.metadata.run(cmd)
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

    pub fn search_pump(&mut self, dt: f32, directory: browse::DirectoryView<'_>) -> bool {
        self.search.pump_with_directory_and_gate(dt, directory, &self.landgate)
    }

    pub fn search_snapshot(&self, directory: browse::DirectoryView<'_>) -> search::SearchSnapshot {
        self.search.snapshot_with_directory(directory)
    }

    /// Controlled discovery against this aggregate's Browse owner.
    pub fn browse_controlled_discover(
        &self,
        launch: &mut dyn FnMut(crate::browse::DiscoveryRequest) -> bool,
    ) {
        self.browse.borrow_mut().controlled_discover(launch);
    }

    /// ViewState's synchronous command path, with every Browse side effect addressed back to this
    /// aggregate. The callback is invoked inline, preserving the press-frame optimistic edit.
    pub fn viewstate_run(
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
    pub fn viewstate_pump(
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

    pub fn take_detail_refresh(&self) -> Option<viewstate::DetailRefresh> {
        self.viewstate.borrow_mut().take_detail_refresh()
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

impl StoreId {
    pub const ALL: [StoreId; 8] = [
        StoreId::Browse,
        StoreId::Hubs,
        StoreId::Metadata,
        StoreId::Search,
        StoreId::Person,
        StoreId::ViewState,
        StoreId::Collection,
        StoreId::SubtitleSearch,
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
/// landing gate's DUMP MODE the take waits, bounded and fail-closed, until the mail the claim owes
/// has been taken, so a request that has not been superseded lands on the site's next execution
/// whatever the worker's timing. Everywhere else it is exactly [`take_landing`]. `owed` must be
/// the claim of the ONE mailbox `f` takes from, never a store-wide one: a store with several
/// mailboxes would wait on the wrong one.
///
/// For a SUPERSEDABLE request, `f` must be [`Fetch::take_current`], not [`Fetch::take`]: the wait
/// ends on the first mail `f` RETURNS, and `take_current` returns only the answer to the claimed
/// generation, so a superseded request's stale answer neither ends the wait nor releases the new
/// request's claim.
pub fn take_landing_owed<T>(gate: &plx_machine::landgate::Gate, id: StoreId,
    owed: impl Fn() -> bool, f: impl FnMut() -> Option<T>) -> Option<T> {
    gate.take_owed(id.ord(), id.name(), owed, f)
}

/// [`take_landings`] for a queue site; see [`take_landing_owed`].
pub fn take_landings_owed<T>(gate: &plx_machine::landgate::Gate, id: StoreId,
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
        slot: std::sync::Mutex::new(None),
    };

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<M>> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Is the claim held? A spawn's first gate.
    pub fn busy(&self) -> bool {
        self.in_flight.load(std::sync::atomic::Ordering::SeqCst)
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
    /// what it will answer about is no longer open.
    pub fn clear(&self) {
        *self.lock() = None;
        self.release();
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

    /// Is mail waiting? A test's view of the mailbox without taking it.
    #[cfg(any(test, feature = "test-support"))]
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
        assert_eq!(StoreId::from_ord(StoreOrd(8)), None);
    }
}
