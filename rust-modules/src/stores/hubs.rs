//! Home's hub catalog, as a machine over `crate::pms` (`docs/stores-as-machines.md`).

use crate::ui::machine::{Cx, Effects, Handled, Host, Machine};

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

pub(crate) fn land(result: &HubsResult) {
    let changed = crate::pms::land(result);
    super::note(StoreId::Hubs, changed);
}

fn tick(dt: f32) {
    let before = crate::pms::catalog_gen();
    crate::pms::tick(dt);
    super::note(StoreId::Hubs, crate::pms::catalog_gen() != before);
}

/// The shim: step the store NOW through the one vocabulary and answer as the mutator did.
pub(crate) fn apply(cmd: HubsCmd) -> bool {
    super::apply(super::StoreCmd::Hubs(cmd))
}

/// The store's own step, reached only through [`super::apply`]. D3 moved the match itself into
/// `pms::run` — its four arms called `pub(crate)` mutators across this module boundary, which is
/// exactly what a new screen could have done too; the mutators are private to `pms.rs` now and
/// this is their only door.
pub(super) fn run(cmd: HubsCmd) -> bool {
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

impl<H: Host> Machine<H> for HubsStore {
    type Ev = StoreEv<HubsCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(c) => {
                run(c.clone());
            }
            StoreEv::Pump { dt } => tick(*dt),
        }
        Handled::Yes
    }
}
