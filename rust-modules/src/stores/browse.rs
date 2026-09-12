//! The Library table and its per-section listing, as a machine over `crate::browse`
//! (`docs/stores-as-machines.md`). The vocabulary is [`BrowseCmd`]; the data and the workers
//! stay in `browse/` until phase 8 moves the screen.

use crate::plex::ServerId;
use crate::ui::machine::{Cx, Effects, Handled, Machine};

use super::{note, StoreEv, StoreId};

pub(crate) use crate::browse::view::{DirectorySnapshot, DirectoryView, ListingSnapshot, ListingView};
pub(crate) use crate::browse::section_hubs::{HubsSnapshot, HubsView};

pub(crate) fn hubs_snapshot() -> HubsSnapshot {
    crate::browse::section_hubs::snapshot(crate::browse::cur())
}

/// Retain the current listing for one dispatcher frame, without copying its items.
pub(crate) fn listing_snapshot() -> ListingSnapshot {
    crate::browse::view::snapshot()
}

/// Every mutation of the browse store a screen may ask for.
#[derive(Clone, Debug)]
pub(crate) enum BrowseCmd {
    Discovery(crate::browse::record::Result),
    RetrySource { epoch: u32, sid: ServerId },
    /// Execute deferred Library work against the source and table epoch captured by the screen.
    Addressed { target: SectionAddress, work: LibraryWork },
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
    SetWatchedLocal { sid: ServerId, rk: String, on: bool },
    LeftTheDeck { sid: ServerId, rk: String },
}

pub(crate) struct BrowseStore;

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
    SaveCursor { query: u32, cursor: crate::browse::Cursor },
    /// Selection and query coexist and commit in this order, inside one store delivery.
    Commit { select: bool, choice: bool, query: Option<QueryEdit> },
    Want { lo: usize, hi: usize },
    Letters,
    Genres,
    Hubs { may_publish: bool },
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
    // `crate::browse`'s statics are a crate global reached from both `apply` above and
    // `crate::stores::apply(StoreCmd::Browse(..))` directly (some fixtures deliver a `StoreCmd`
    // without going through this module's `apply`) — guard the one point both funnel through. See
    // `lib.rs::testlock` and D5.
    #[cfg(test)]
    crate::testlock::assert_held("the browse store (apply)");
    crate::browse::run(cmd)
}

/// The landing pass the Library screen runs once a frame while it is up: pages, menu data, the
/// roster, the shelves. Answers `true` when the store changed.
pub(crate) fn pump() -> super::StoreOutcome {
    let outcome = crate::browse::pump();
    note(StoreId::Browse, outcome.changed);
    outcome
}

/// The roster half alone — what Home and Search run to learn about a friend's libraries without
/// fetching any page.
pub(crate) fn discover_pump() -> super::EndpointRefreshSet {
    crate::browse::discover_pump()
}

impl<H: super::StoreEffectHost> Machine<H> for BrowseStore {
    type Ev = StoreEv<BrowseCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(c) => {
                run(c.clone());
            }
            StoreEv::Pump { .. } => {
                pump().endpoints.emit(fx);
            }
        }
        Handled::Yes
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;

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
        let own = crate::plex::register_for_test("switch-own", "127.0.0.1", 9, "synthetic", "fixture");
        let shared = crate::plex::register_for_test("switch-shared", "127.0.0.1", 10, "synthetic", "fixture");
        crate::browse::seed_registered_table_for_test([own, shared]);
        apply(BrowseCmd::SetCur(0));
        let a = SectionAddress { epoch: crate::browse::table_epoch(), sid: own, section: 1 };
        let b = SectionAddress { sid: shared, ..a };
        assert_eq!(crate::browse::resolve_section(b.epoch, b.sid, b.section), Some(2));
        let commit = |target, select, choice| apply(BrowseCmd::Addressed {
            target, work: LibraryWork::Commit { select, choice, query: None },
        });
        let observe = |target, select, choice| crate::diag::test_events::capture(|| commit(target, select, choice));
        let switched = crate::diag::schema::DiagEvent::FeatureUsed {
            feature: crate::diag::schema::Feature::LibrarySwitch,
        };

        // Same-current includes the final A commit after a pending A→B→A was superseded.
        assert_eq!(observe(a, true, true), (true, vec![]));
        assert_eq!(observe(SectionAddress { section: 999, ..b }, true, true), (false, vec![]));
        assert_eq!(observe(SectionAddress { epoch: b.epoch.wrapping_add(1), ..b }, true, true), (false, vec![]));
        assert_eq!(observe(b, false, true), (false, vec![]), "foreign work without selection cannot count");
        assert_eq!(crate::browse::cur(), 0);
        assert_eq!(observe(b, true, false), (true, vec![]), "boot/repoint is not a viewer choice");
        assert_eq!(crate::browse::cur(), 2);
        assert_eq!(observe(b, true, true), (true, vec![]), "choosing the current library is quiet");
        assert_eq!(observe(a, true, true), (true, vec![switched]));
        assert_eq!(crate::browse::cur(), 0);
        assert_eq!(observe(a, true, true), (true, vec![]), "repeated delivery cannot count twice");
        assert_eq!(observe(b, true, true), (true, vec![switched]), "a later real switch counts once");
        let (accepted, events) = crate::diag::test_events::capture(|| apply(BrowseCmd::Addressed {
            target: b, work: LibraryWork::Commit { select: false, choice: false,
                query: Some(QueryEdit::Unwatched(true)) },
        }));
        assert!(accepted);
        assert!(events.is_empty(), "query changes are not library switches");
    }
}
