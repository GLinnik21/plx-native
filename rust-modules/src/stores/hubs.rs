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

/// The shim: step the store NOW through the one vocabulary and answer as the mutator did.
pub(crate) fn apply(cmd: HubsCmd) -> bool {
    super::apply(super::StoreCmd::Hubs(cmd))
}

/// The store's own step, reached only through [`super::apply`].
pub(super) fn run(cmd: HubsCmd) -> bool {
    let answer = match cmd {
        HubsCmd::RefetchHubs => {
            crate::pms::request_refetch_hubs();
            true
        }
        HubsCmd::Retry => {
            crate::pms::request_retry();
            true
        }
        HubsCmd::Reset => {
            crate::pms::reset();
            true
        }
        HubsCmd::EditItem { sid, rk, edit } => crate::pms::edit_item(sid, &rk, edit),
    };
    super::bump(StoreId::Hubs);
    answer
}

/// Home's once-a-frame pass: land, back off, refetch. `pms::pump` reports no change of its own,
/// so the notice is raised on its catalog generation moving instead.
pub(crate) fn pump(dt: f32) {
    let before = crate::pms::catalog_gen();
    crate::pms::pump(dt);
    super::note(StoreId::Hubs, crate::pms::catalog_gen() != before);
}

impl<H: Host> Machine<H> for HubsStore {
    type Ev = StoreEv<HubsCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(c) => {
                run(c.clone());
            }
            StoreEv::Pump { dt } => pump(*dt),
        }
        Handled::Yes
    }
}
