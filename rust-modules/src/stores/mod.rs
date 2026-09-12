//! **Stores as machines** (restructure spec §2.1/§2.2, phase 4; `docs/stores-as-machines.md`).
//!
//! Six data modules own the application's server-derived state — `browse`, `pms` (the Home
//! hubs), `metadata`, `search`, `person`, `viewstate` — and every one is still the `static mut`
//! + mailbox + `pump()` shape it was born with. This layer puts ONE entrance in front of each:
//! a [`StoreCmd`] is the complete, enumerated vocabulary of mutations, a store's `Machine::step`
//! is the one place a legacy mutator is called, and every applied command or changed landing
//! raises the store's NOTICE (a generation the shadow dispatcher delivers to every live
//! instance as `ScreenEvent::StoreChanged`, spec §3.4).
//!
//! Two callers, one `step`. A migrated screen emits `AppFx::Store(id, cmd)` and the shadow rig
//! delivers it (`app/legacy.rs`); a legacy screen calls the store's `apply(cmd)` shim, which
//! steps the same machine IMMEDIATELY on the main thread and returns the store's own answer —
//! the design note's §3 says why the shim is not a deferred queue in this phase. Either way the
//! mutator has one caller and the notice is raised once.
//!
//! What lives here is the VOCABULARY and the machine; the data stays in the legacy modules until
//! each screen's phase moves it (§14). This module names data crates, `ui::machine` and — since
//! phase 11's landing schedule — `ui::landgate`, and nothing else (spec §2.1's layer rule;
//! `ci/check-deps.sh`'s `mutators` gate refuses the old spelling outside `stores/` and the data
//! modules).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

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
    /// The legacy `pump()`: land whatever arrived. `dt` for the pumps that debounce on it.
    /// Constructed by the dispatcher path once a store is stepped on `Tick` (a migrated screen's
    /// phase); the legacy loop calls the store's `pump` fn directly.
    #[allow(dead_code)]
    Pump { dt: f32 },
}

/// Apply ONE command to whichever store it names — THE dispatch. Every shim (`browse::apply`
/// and its five siblings) wraps its command into the vocabulary and comes through here, and the
/// dispatcher path steps the same `run` through the store's `Machine::step`; a trace or a
/// recorder hook for store mutations has exactly one place to stand.
pub(crate) fn apply(cmd: StoreCmd) -> StoreOutcome {
    match cmd {
        StoreCmd::Browse(c) => StoreOutcome::changed(browse::run(c)),
        StoreCmd::Hubs(c) => hubs::run(c),
        StoreCmd::Metadata(c) => StoreOutcome::changed(metadata::run(c)),
        StoreCmd::Search(c) => StoreOutcome::changed(search::run(c)),
        StoreCmd::Person(c) => StoreOutcome::changed(person::run(c)),
        StoreCmd::ViewState(c) => StoreOutcome::changed(viewstate::run(c)),
    }
}

// ---------------------------------------------------------------------------------------------
// the notice: one generation per store, marked dirty by every applied command and every
// changed landing, drained once a frame into the shadow dispatcher
// ---------------------------------------------------------------------------------------------

struct Notice {
    gen: AtomicU32,
    dirty: AtomicBool,
}

const fn notice() -> Notice {
    Notice {
        gen: AtomicU32::new(0),
        dirty: AtomicBool::new(false),
    }
}

/// Atomics rather than `static mut`: they are read and written on the main thread only, but an
/// atomic needs no `unsafe` block at the ~40 call sites and is what the legacy stores already use
/// for their own generations.
static NOTICES: [Notice; 6] = [notice(), notice(), notice(), notice(), notice(), notice()];

/// The store changed: bump its generation and owe a notice.
pub(crate) fn bump(id: StoreId) -> u32 {
    let n = &NOTICES[id as usize];
    n.dirty.store(true, Ordering::Relaxed);
    n.gen.fetch_add(1, Ordering::Relaxed) + 1
}

/// The store's generation — what a migrated screen keys a `Memo` on (first reader: 5b).
#[allow(dead_code)]
pub(crate) fn gen(id: StoreId) -> u32 {
    NOTICES[id as usize].gen.load(Ordering::Relaxed)
}

/// Drain the owed notices, once a frame at the loop's drain point (`app/legacy.rs::mirror`).
pub(crate) fn take_notices() -> Vec<(StoreId, u32)> {
    let mut out = Vec::new();
    for id in StoreId::ALL {
        let n = &NOTICES[id as usize];
        if n.dirty.swap(false, Ordering::Relaxed) {
            out.push((id, n.gen.load(Ordering::Relaxed)));
        }
    }
    out
}

/// A pump's answer folded into the notice: `true` bumps.
fn note(id: StoreId, changed: bool) -> bool {
    if changed {
        bump(id);
    }
    changed
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
        let before = gen(StoreId::Search);
        assert!(apply(StoreCmd::Search(search::SearchCmd::Reset)).changed);
        let n = take_notices();
        assert!(n.contains(&(StoreId::Search, before + 1)), "{n:?}");
        assert!(take_notices().is_empty(), "drained once");
    }

    #[test]
    fn the_ordinal_round_trips_for_every_store() {
        for id in StoreId::ALL {
            assert_eq!(StoreId::from_ord(id.ord()), Some(id));
        }
        assert_eq!(StoreId::from_ord(StoreOrd(6)), None);
    }
}
