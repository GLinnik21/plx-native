//! The person page's data layer, as a machine over `crate::person` (`docs/stores-as-machines.md`).

use crate::plex::ServerId;
use crate::ui::machine::{Cx, Effects, Handled, Host, Machine};

use super::{note, StoreEv, StoreId};

#[derive(Clone, Debug)]
pub(crate) enum PersonCmd {
    /// Mount a person from the header a cast row handed in; the fetches spawn on the next pump.
    Open {
        sid: ServerId,
        key: String,
        guid: String,
        name: String,
        thumb: String,
    },
    Close,
    /// The profile/account switch.
    Reset,
    /// The optimistic half of a view-state write, on the person's shelves.
    SetWatchedLocal { sid: ServerId, rk: String, on: bool },
}

pub(crate) struct PersonStore;

/// The shim: step the store NOW through the one vocabulary and answer as the mutator did.
pub(crate) fn apply(cmd: PersonCmd) -> bool {
    super::apply(super::StoreCmd::Person(cmd))
}

/// The store's own step, reached only through [`super::apply`]. D3 moved the match itself into
/// `person::run` — its arms called `pub(crate)` mutators (`open`, `close`, `reset`,
/// `set_watched_local`) across this module boundary; those four are private to `person.rs` now
/// and this is their only door.
pub(super) fn run(cmd: PersonCmd) -> bool {
    // `crate::person`'s statics are a crate global reached from both `apply` above and
    // `crate::stores::apply(StoreCmd::Person(..))` directly (some fixtures deliver a `StoreCmd`
    // without going through this module's `apply`) — guard the one point both funnel through. See
    // `lib.rs::testlock` and D5.
    #[cfg(test)]
    crate::testlock::assert_held("the person store (apply)");
    let answer = crate::person::run(cmd);
    super::bump(StoreId::Person);
    answer
}

/// The page's once-a-frame pass: land every fetch, schedule the next.
pub(crate) fn pump() -> bool {
    note(StoreId::Person, crate::person::pump())
}

impl<H: Host> Machine<H> for PersonStore {
    type Ev = StoreEv<PersonCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(c) => {
                run(c.clone());
            }
            StoreEv::Pump { .. } => {
                pump();
            }
        }
        Handled::Yes
    }
}
