//! The view-state WRITE queue, as a machine over `crate::viewstate` (`docs/stores-as-machines.md`).

use crate::plex::ServerId;
use crate::ui::machine::{Cx, Effects, Handled, Host, Machine};

use super::{StoreEv, StoreId};

#[derive(Clone, Debug)]
pub(crate) enum ViewStateCmd {
    /// Ask the item's server to change `(sid, rk)`'s view state; every local surface flips at
    /// once. Answers `false` when the write never left (an unregistered server slot).
    Request {
        sid: ServerId,
        rk: String,
        write: crate::viewstate::Write,
        /// What to re-read when it lands — see `viewstate::request`.
        detail: Option<String>,
        guid: String,
    },
    /// The profile/account switch.
    Reset,
}

pub(crate) struct ViewStateStore;

/// The shim: step the store NOW through the one vocabulary and answer as the mutator did.
pub(crate) fn apply(cmd: ViewStateCmd) -> bool {
    super::apply(super::StoreCmd::ViewState(cmd))
}

/// The store's own step, reached only through [`super::apply`]. D3 moved the match itself into
/// `viewstate::run` — its arms called `pub(crate)` mutators (`request`, `reset`) across this
/// module boundary; those two are private to `viewstate.rs` now and this is their only door.
pub(super) fn run(cmd: ViewStateCmd) -> bool {
    // `crate::viewstate`'s statics are a crate global reached from both `apply` above and
    // `crate::stores::apply(StoreCmd::ViewState(..))` directly (some fixtures deliver a
    // `StoreCmd` without going through this module's `apply`) — guard the one point both funnel
    // through. See `lib.rs::testlock` and D5.
    #[cfg(test)]
    crate::testlock::assert_held("the viewstate store (apply)");
    let answer = crate::viewstate::run(cmd);
    super::bump(StoreId::ViewState);
    answer
}

/// The route-unconditional landing the loop runs every frame.
pub(crate) fn pump() {
    let busy = crate::viewstate::is_busy();
    crate::viewstate::pump();
    // a landing is what turns "busy" off; the refresh it owes is raised through the stores it
    // touches (hubs, the detail re-read), so the notice here is the queue's own state
    super::note(StoreId::ViewState, busy != crate::viewstate::is_busy());
}

/// `crate::viewstate::take_detail_refresh`'s door (D3): the frame loop used to call that
/// `pub(crate)` fn directly, naming `crate::viewstate::` rather than going through this store —
/// the one confirmed production bypass the D3 census found. The drain itself stays in
/// `viewstate.rs` (it only reads state [`pump`] above already owns, on the main thread, once a
/// frame — a landing-adjacent door, not a screen-facing mutator); this is just the sanctioned
/// path to it.
pub(crate) fn take_detail_refresh() -> Option<String> {
    crate::viewstate::take_detail_refresh()
}

impl<H: Host> Machine<H> for ViewStateStore {
    type Ev = StoreEv<ViewStateCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(c) => {
                run(c.clone());
            }
            StoreEv::Pump { .. } => pump(),
        }
        Handled::Yes
    }
}
