//! The Search data layer, as a machine over `crate::search` (`docs/stores-as-machines.md`).

use crate::ui::machine::{Cx, Effects, Handled, Host, Machine};

use super::{note, StoreEv, StoreId};

#[derive(Clone, Debug)]
pub(crate) enum SearchCmd {
    /// The field's text; a change of the TRIMMED terms supersedes the answer and restarts the
    /// debounce, a change of whitespace only repaints.
    SetQuery(String),
    /// Sign-out / profile switch: drop the query and every shelf.
    Reset,
    /// The optimistic half of a view-state write, on the result shelves.
    SetWatchedLocal { sid: crate::plex::ServerId, rk: String, on: bool },
}

pub(crate) struct SearchStore;

/// The shim: step the store NOW through the one vocabulary and answer as the mutator did.
pub(crate) fn apply(cmd: SearchCmd) -> bool {
    super::apply(super::StoreCmd::Search(cmd))
}

/// The store's own step, reached only through [`super::apply`].
pub(super) fn run(cmd: SearchCmd) -> bool {
    let answer = match cmd {
        SearchCmd::SetQuery(q) => {
            crate::search::set_query(&q);
            true
        }
        SearchCmd::Reset => {
            crate::search::reset();
            true
        }
        SearchCmd::SetWatchedLocal { sid, rk, on } => crate::search::set_watched_local(sid, &rk, on),
    };
    super::bump(StoreId::Search);
    answer
}

/// The screen's once-a-frame pass: the debounce, the spawns, the landings.
pub(crate) fn pump(dt: f32) -> bool {
    note(StoreId::Search, crate::search::pump(dt))
}

impl<H: Host> Machine<H> for SearchStore {
    type Ev = StoreEv<SearchCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(c) => {
                run(c.clone());
            }
            StoreEv::Pump { dt } => {
                pump(*dt);
            }
        }
        Handled::Yes
    }
}
