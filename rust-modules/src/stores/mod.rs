//! **Stores as machines** (restructure spec §2.1/§2.2, phase 4; `docs/stores-as-machines.md`).
//!
//! Six data modules provide the application's server-derived state — `browse`, `pms` (the Home
//! hubs), `metadata`, `search`, `person`, `viewstate`. Browse, Person and ViewState are physically owned by
//! one [`Stores`] aggregate per `crate::app::bridge::Bridge`, with per-instance state, adapter and
//! notice; the other three retain the compatibility global + mailbox shape. This layer puts ONE entrance in
//! front of each: a [`StoreCmd`] is the complete, enumerated vocabulary of mutations, a store's
//! owned command decoder is the one place its vocabulary is applied, and every command that changes
//! observable state or landing that changes the store raises the store's NOTICE (a generation the
//! bridge dispatcher delivers to every live instance as `ScreenEvent::StoreChanged`, spec §3.4).
//!
//! Owned stores have two caller shapes and one explicit owner: screens emit `AppFx::Store(id, cmd)` for
//! `app/bridge.rs` to deliver, while same-turn application boundaries call a method on the
//! [`Stores`] value they already hold. The generic `apply(cmd)` dispatcher remains only for the
//! three not-yet-owned stores and rejects Browse, Person and ViewState commands.
//!
//! What lives here is the vocabulary and machines plus Browse/Person/ViewState's production aggregate; the remaining
//! data stays in the legacy modules until its ownership slice (§14). This module names data crates, `ui::machine` and — since
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
pub(crate) mod hubs;
pub(crate) mod metadata;
pub(crate) mod person;
pub(crate) mod search;
pub(crate) mod viewstate;

/// Production store aggregate. Browse, Person and ViewState are physical owners here; the remaining stores
/// retain their compatibility owners until their corresponding ownership slices land.
pub(crate) struct Stores {
    pub(crate) browse: std::rc::Rc<std::cell::RefCell<browse::BrowseStore>>,
    pub(crate) hubs: hubs::HubsStore,
    pub(crate) metadata: metadata::MetadataStore,
    pub(crate) person: person::PersonStore,
    pub(crate) search: search::SearchStore,
    pub(crate) viewstate: std::cell::RefCell<viewstate::ViewStateStore>,
}

impl Default for Stores {
    fn default() -> Self {
        let browse = std::rc::Rc::new(std::cell::RefCell::new(browse::BrowseStore::default()));
        Self {
            browse,
            hubs: hubs::HubsStore::default(),
            metadata: metadata::MetadataStore::default(),
            person: person::PersonStore::default(),
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
        self.browse.borrow_mut().discover_pump()
    }

    pub(crate) fn person_run(&mut self, cmd: person::PersonCmd) -> bool {
        self.person.run(cmd)
    }

    pub(crate) fn person_pump(&mut self) -> bool {
        self.person.pump()
    }

    pub(crate) fn person_view(&self) -> crate::person::PersonView<'_> {
        self.person.view()
    }

    pub(crate) fn metadata_run(&mut self, cmd: metadata::MetadataCmd) -> bool {
        self.metadata.run(cmd)
    }

    pub(crate) fn metadata_pump(&mut self) -> bool {
        self.metadata.pump()
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
        self.search.pump_with_directory(dt, directory)
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
        let search = &mut self.search;
        let metadata = &mut self.metadata;
        self.viewstate.borrow_mut().run(
            cmd,
            &mut |cmd| browse.borrow_mut().run(cmd),
            &mut |hubcmd| hubs.run_with_directory(hubcmd, directory),
            &mut |cmd| person.run(cmd),
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
        let search = &mut self.search;
        let metadata = &mut self.metadata;
        self.viewstate.borrow_mut().pump(
            &mut |cmd| browse.borrow_mut().run(cmd),
            &mut |hubcmd| hubs.run_with_directory(hubcmd, directory),
            &mut |cmd| person.run(cmd),
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
    pub(crate) const ALL: [StoreId; 6] = [
        StoreId::Browse,
        StoreId::Hubs,
        StoreId::Metadata,
        StoreId::Search,
        StoreId::Person,
        StoreId::ViewState,
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

/// A one-slot mailbox: `None` while the replay is still waiting for this store's recorded frame.
pub(crate) fn take_landing<T>(id: StoreId, f: impl FnMut() -> Option<T>) -> Option<T> {
    crate::ui::landgate::take(id.ord(), f)
}

/// A mailbox drained as a QUEUE: an empty answer is not a landing.
pub(crate) fn take_landings<T>(id: StoreId, f: impl FnMut() -> Vec<T>) -> Vec<T> {
    crate::ui::landgate::take_all(id.ord(), f)
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

    #[test]
    fn a_command_raises_one_notice_and_a_steady_store_none() {
        let _g = crate::testlock::serial();
        let _ = take_notices();
        let before = gen(StoreId::Metadata);
        assert!(apply(StoreCmd::Metadata(metadata::MetadataCmd::Clear)).changed);
        let n = take_notices();
        assert!(n.contains(&(StoreId::Metadata, before + 1)), "{n:?}");
        assert!(take_notices().is_empty(), "drained once");
    }

    #[test]
    fn generic_dispatch_rejects_all_physically_owned_stores() {
        for command in [
            StoreCmd::Browse(browse::BrowseCmd::Reset),
            StoreCmd::Person(person::PersonCmd::Reset),
            StoreCmd::Search(search::SearchCmd::Reset),
            StoreCmd::ViewState(viewstate::ViewStateCmd::Reset),
        ] {
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| apply(command))).is_err());
        }
    }

    #[test]
    fn the_ordinal_round_trips_for_every_store() {
        for id in StoreId::ALL {
            assert_eq!(StoreId::from_ord(id.ord()), Some(id));
        }
        assert_eq!(StoreId::from_ord(StoreOrd(6)), None);
    }
}
