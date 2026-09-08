//! The Library table and its per-section listing, as a machine over `crate::browse`
//! (`docs/stores-as-machines.md`). The vocabulary is [`BrowseCmd`]; the data and the workers
//! stay in `browse/` until phase 8 moves the screen.

use crate::plex::ServerId;
use crate::ui::machine::{Cx, Effects, Handled, Host, Machine};

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
    /// Execute deferred Library work against the source and table epoch captured by the screen.
    Addressed { target: SectionAddress, work: LibraryWork },
    /// Point the listing at section `i` (a pill or library-row press, committed at the fade floor).
    SetCur(usize),
    /// Remember a library the viewer CHOSE (never a boot settle or a re-point).
    NoteLibraryChoice(usize),
    KickLetters,
    KickGenres,
    /// The grid's wanted index window — drives which page fetches next.
    Want { lo: usize, hi: usize },
    SaveView { focus: usize, scroll: f32 },
    /// Answers `false` when the key names no sort entry.
    SetSortByKey { key: String, desc: bool },
    ToggleUnwatched,
    /// `None` is "All genres"; answers `false` when the id names no genre.
    SetGenreById(Option<String>),
    RetryCurSource,
    RecheckShares,
    /// The Home editor's draft commit: one record for the whole session.
    ApplyPins(Vec<(usize, bool)>),
    RetryDiscovery,
    /// The profile/account switch: wipe everything and supersede everything in flight.
    Reset,
    /// The library's own shelves (`browse::section_hubs`).
    HubsKick(usize),
    HubsCommitStaged { sec: usize, may_move: bool },
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
    /// Selection and query coexist and commit in this order, inside one store delivery.
    Commit { select: bool, choice: bool, query: Option<QueryEdit> },
    Want { lo: usize, hi: usize },
    Letters,
    Genres,
    Hubs { may_publish: bool },
    Retry,
}

fn addressed(target: SectionAddress, work: LibraryWork) -> bool {
    let Some(index) = crate::browse::resolve_section(target.epoch, target.sid, target.section) else { return false };
    match work {
        LibraryWork::Commit { select, choice, query } => {
            if select { crate::browse::set_cur(index); }
            if crate::browse::cur() != index { return false; }
            if choice { crate::browse::note_library_choice(index); }
            match query {
                Some(QueryEdit::Sort { key, desc }) => crate::browse::set_sort_by_key(&key, desc),
                Some(QueryEdit::Unwatched(on)) => crate::browse::set_unwatched(on),
                Some(QueryEdit::Genre(id)) => crate::browse::set_genre_by_id(id.as_deref()),
                None => true,
            }
        }
        LibraryWork::Hubs { may_publish } => {
            crate::browse::section_hubs::kick(index);
            crate::browse::section_hubs::commit_staged(index, may_publish)
        }
        work => {
            if crate::browse::cur() != index { return false; }
            match work {
                LibraryWork::Want { lo, hi } => crate::browse::want(lo, hi),
                LibraryWork::Letters => crate::browse::kick_letters(),
                LibraryWork::Genres => crate::browse::kick_genres(),
                LibraryWork::Retry => crate::browse::retry_cur_source(),
                LibraryWork::Commit { .. } | LibraryWork::Hubs { .. } => unreachable!(),
            }
            true
        }
    }
}

/// The shim: step the store NOW through the one vocabulary and answer as the mutator did.
pub(crate) fn apply(cmd: BrowseCmd) -> bool {
    super::apply(super::StoreCmd::Browse(cmd))
}

/// The store's own step, reached only through [`super::apply`].
pub(super) fn run(cmd: BrowseCmd) -> bool {
    let answer = match cmd {
        BrowseCmd::Addressed { target, work } => addressed(target, work),
        BrowseCmd::SetCur(i) => {
            crate::browse::set_cur(i);
            true
        }
        BrowseCmd::NoteLibraryChoice(i) => {
            crate::browse::note_library_choice(i);
            true
        }
        BrowseCmd::KickLetters => {
            crate::browse::kick_letters();
            true
        }
        BrowseCmd::KickGenres => {
            crate::browse::kick_genres();
            true
        }
        BrowseCmd::Want { lo, hi } => {
            crate::browse::want(lo, hi);
            true
        }
        BrowseCmd::SaveView { focus, scroll } => {
            crate::browse::save_view(focus, scroll);
            true
        }
        BrowseCmd::SetSortByKey { key, desc } => crate::browse::set_sort_by_key(&key, desc),
        BrowseCmd::ToggleUnwatched => {
            crate::browse::toggle_unwatched();
            true
        }
        BrowseCmd::SetGenreById(id) => crate::browse::set_genre_by_id(id.as_deref()),
        BrowseCmd::RetryCurSource => {
            crate::browse::retry_cur_source();
            true
        }
        BrowseCmd::RecheckShares => {
            crate::browse::recheck_shares();
            true
        }
        BrowseCmd::ApplyPins(edits) => {
            crate::browse::apply_pins(&edits);
            true
        }
        BrowseCmd::RetryDiscovery => {
            crate::browse::retry_discovery();
            true
        }
        BrowseCmd::Reset => {
            crate::browse::reset();
            true
        }
        BrowseCmd::HubsKick(sec) => {
            crate::browse::section_hubs::kick(sec);
            true
        }
        BrowseCmd::HubsCommitStaged { sec, may_move } => crate::browse::section_hubs::commit_staged(sec, may_move),
        BrowseCmd::HubsInvalidateAll => {
            crate::browse::section_hubs::invalidate_all();
            true
        }
        BrowseCmd::SetWatchedLocal { sid, rk, on } => {
            let a = crate::browse::set_watched_local(sid, &rk, on);
            let b = crate::browse::section_hubs::set_watched_local(sid, &rk, on);
            a || b
        }
        BrowseCmd::LeftTheDeck { sid, rk } => crate::browse::section_hubs::left_the_deck(sid, &rk),
    };
    // a command that changed nothing (an unknown sort key, a no-op pin batch) still moved the
    // store's request state in every other arm, and a screen keying a memo on the generation
    // must see every applied command — so every command bumps
    super::bump(StoreId::Browse);
    answer
}

/// The landing pass the Library screen runs once a frame while it is up: pages, menu data, the
/// roster, the shelves. Answers `true` when the store changed.
pub(crate) fn pump() -> bool {
    note(StoreId::Browse, crate::browse::pump())
}

/// The roster half alone — what Home and Search run to learn about a friend's libraries without
/// fetching any page.
pub(crate) fn discover_pump() {
    crate::browse::discover_pump();
}

impl<H: Host> Machine<H> for BrowseStore {
    type Ev = StoreEv<BrowseCmd>;
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
