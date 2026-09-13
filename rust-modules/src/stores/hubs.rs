//! Home's hub catalog, as a machine over `crate::pms` (`docs/stores-as-machines.md`).

use crate::ui::machine::{Cx, Effects, Handled, Machine};

use super::{StoreEv, StoreId};

#[derive(Clone, Debug)]
pub(crate) enum HubsCmd {
    /// A refetch is owed (a view-state write landed, a profile settled).
    RefetchHubs,
    /// The read-out's Retry: clear the back-off and ask again.
    Retry,
    /// The profile/account switch.
    Reset,
    /// The optimistic half of a view-state write on the hub catalog (`pms::LocalEdit`).
    EditItem { sid: crate::plex::ServerId, rk: String, edit: crate::pms::LocalEdit },
}

pub(crate) struct HubsStore;

pub(crate) use crate::pms::Landing as HubsResult;

/// The adapter boundary: drain owned results without applying any store state.
pub(crate) fn take_results() -> Vec<HubsResult> {
    crate::pms::take_landings()
}

pub(crate) fn land(result: &HubsResult) -> super::StoreOutcome {
    let outcome = crate::pms::land(result);
    super::note(StoreId::Hubs, outcome.changed);
    outcome
}

fn tick(dt: f32) -> super::StoreOutcome {
    let before = crate::pms::catalog_gen();
    let endpoints = crate::pms::tick(dt);
    let changed = super::note(StoreId::Hubs, crate::pms::catalog_gen() != before);
    super::StoreOutcome { changed, endpoints }
}

/// Same mutation and notice rules as run/tick, with an application-owned resource executor.
#[allow(dead_code)] // Compatibility controlled path without an explicit retained directory.
pub(crate) fn controlled(cmd: Option<HubsCmd>, dt: f32,
    launch: &mut dyn FnMut(crate::pms::HubRequest) -> bool) -> super::StoreOutcome {
    #[cfg(test)]
    crate::testlock::assert_held("controlled hubs store");
    let command = cmd.is_some();
    let outcome = crate::pms::controlled_work(cmd,dt,launch);
    if command { super::bump(StoreId::Hubs); }
    else { super::note(StoreId::Hubs,outcome.changed); }
    outcome
}

/// Controlled Home work scoped by the Bridge's retained Browse directory. The retained view is
/// the decision input for this frame; no active-owner selector is consulted.
pub(crate) fn controlled_with_directory(cmd: Option<HubsCmd>, dt: f32,
    directory: crate::stores::browse::DirectoryView<'_>,
    launch: &mut dyn FnMut(crate::pms::HubRequest) -> bool) -> super::StoreOutcome {
    #[cfg(test)]
    crate::testlock::assert_held("controlled hubs store with Browse owner");
    let command = cmd.is_some();
    let outcome = crate::pms::controlled_work_with_directory(cmd, dt, directory, launch);
    if command { super::bump(StoreId::Hubs); }
    else { super::note(StoreId::Hubs, outcome.changed); }
    outcome
}

/// The shim: step the store NOW through the one vocabulary and answer as the mutator did.
pub(crate) fn apply(cmd: HubsCmd) -> super::StoreOutcome {
    super::apply(super::StoreCmd::Hubs(cmd))
}

/// The store's own step, reached only through [`super::apply`]. D3 moved the match itself into
/// `pms::run` — its four arms called `pub(crate)` mutators across this module boundary, which is
/// exactly what a new screen could have done too; the mutators are private to `pms.rs` now and
/// this is their only door.
pub(super) fn run(cmd: HubsCmd) -> super::StoreOutcome {
    // `crate::pms`'s statics are a crate global reached from both `apply` above and
    // `crate::stores::apply(StoreCmd::Hubs(..))` directly (some fixtures deliver a `StoreCmd`
    // without going through this module's `apply`) — guard the one point both funnel through, even
    // though `pms::run`'s own arms each delegate to a `crate::pms` function that asserts on its
    // own. See `lib.rs::testlock` and D5.
    #[cfg(test)]
    crate::testlock::assert_held("the hubs store (apply)");
    let answer = crate::pms::run(cmd);
    super::bump(StoreId::Hubs);
    answer
}

impl<H: super::StoreEffectHost> Machine<H> for HubsStore {
    type Ev = StoreEv<HubsCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let outcome = match ev {
            StoreEv::Cmd(c) => run(c.clone()),
            StoreEv::Pump { dt } => tick(*dt),
        };
        outcome.endpoints.emit(fx);
        Handled::Yes
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    fn section(sid: crate::plex::ServerId, section: usize, pinned: bool)
        -> crate::stores::browse::SectionView {
        crate::stores::browse::SectionView {
            borrowed: false,
            sid: Some(sid),
            key: section as i64 + 1,
            kind: crate::stores::browse::SecKind::Movie,
            row: crate::stores::browse::SrcRow {
                section,
                title: format!("Library {section}"),
                pinned,
                current: section == 0,
                ..Default::default()
            },
        }
    }

    #[test]
    fn controlled_hubs_uses_the_supplied_directory_instead_of_active_browse() {
        let _guard = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        let own = crate::plex::register_for_test(
            "hubs-owned", "127.0.0.1", 9, "synthetic", "fixture");
        let hidden = crate::plex::register_for_test(
            "hubs-hidden", "127.0.0.1", 10, "synthetic", "fixture");
        let directory = crate::stores::browse::DirectorySnapshot::fixture(
            7, 0, vec![section(own, 0, true), section(hidden, 1, false)]);
        let mut ignored = |_| false;
        let _ = controlled_with_directory(Some(HubsCmd::Reset), 0.0, directory.view(), &mut ignored);
        let mut launched = Vec::new();

        let _ = controlled_with_directory(Some(HubsCmd::RefetchHubs), 0.0, directory.view(),
            &mut |request| {
                launched.push(request.descriptor().2);
                false
            });

        assert_eq!(launched, [own.raw()],
            "the retained pin table excludes the unpinned source even when compatibility Browse is empty");
        let _ = controlled_with_directory(Some(HubsCmd::Reset), 0.0, directory.view(), &mut ignored);
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
    }
}
