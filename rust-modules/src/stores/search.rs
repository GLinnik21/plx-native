//! The Search data layer, as a machine over `crate::search` (`docs/stores-as-machines.md`).

use crate::ui::machine::{Cx, Effects, Handled, Host, Machine};

use super::{note, StoreEv, StoreId};

pub(crate) use crate::search::view::SearchSnapshot;

/// Capture the store publication at the dispatcher frame boundary, not during paint.
pub(crate) fn snapshot() -> SearchSnapshot { crate::search::view::snapshot() }

#[derive(Clone, Debug)]
pub(crate) enum SearchCmd {
    /// The field's text; a change of the TRIMMED terms supersedes the answer and restarts the
    /// debounce, a change of whitespace only repaints.
    SetQuery(String),
    /// An owned draft cannot submit into a replacement profile's Search store.
    SetQueryScoped { profile_generation: u32, query: String },
    /// A submitted search, not each keystroke; history is scoped to the active profile.
    RememberRecent { profile_generation: u32, term: String },
    ClearRecents { profile_generation: u32 },
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

/// The store's own step, reached only through [`super::apply`]. D3 moved the match itself into
/// `search::run` — its arms called `pub(crate)` mutators (`set_query`, `reset`,
/// `set_watched_local`) across this module boundary; those three are private to `search.rs`
/// now and this is their only door.
pub(super) fn run(cmd: SearchCmd) -> bool {
    // `crate::search`'s statics are a crate global reached from both `apply` above and
    // `crate::stores::apply(StoreCmd::Search(..))` directly (the dispatcher's own delivery path,
    // which some fixtures use without going through this module's `apply`) — guard the one point
    // both funnel through, not each caller, so a test that writes it outside
    // `crate::testlock::serial()` panics HERE rather than corrupting a bystander test. See
    // `lib.rs::testlock` and D5.
    #[cfg(test)]
    crate::testlock::assert_held("the search store (apply)");
    let answer = crate::search::run(cmd);
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
