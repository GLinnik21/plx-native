//! viewledger — the order in which view-state edits were published, and the reads they overtook.
//!
//! [`crate::viewstate::publish_view`] edits what every store HOLDS the instant the user presses.
//! It cannot edit what a store has not received yet. A page, hub, search, filmography or detail
//! read that was already out when the press happened describes the server from BEFORE it, so it
//! lands carrying the pre-edit body and quietly undoes the press (a tick that vanishes, a resume
//! bar that returns) until some later read corrects it. Home alone used to guard against this, with
//! a private replay list; every other store simply lost the race.
//!
//! The ledger is the one general answer, in two halves a store uses:
//!
//! * **Stamp at request time.** A store asks [`ViewLedger::stamp`] when it issues a read and keeps
//!   the number with the read's other bookkeeping (the same place it keeps the read's generation).
//! * **Patch on landing.** When the read comes back the store hands the rows it is about to place to
//!   [`ViewLedger::patch`] (or walks [`ViewLedger::since`] itself where its rows are not
//!   `PmsMovie`s). Every edit published AFTER the stamp is made again on those rows, in the order
//!   it was published; an edit published before the stamp is not, because the server had it when
//!   it answered.
//!
//! ## What is kept
//!
//! One entry per distinct (item, scope, kind of edit) — the LATEST one — each carrying the
//! sequence number it was published at. A second press on the same item replaces the first, so the
//! map is bounded by the items touched in the session rather than by presses. Replaying in
//! sequence order keeps the final state exact: of two entries on different keys the later still
//! replays later, and two entries on one key were redundant by construction (the newest wins).
//!
//! It records CHANGES only. It cannot say what an item looked like before the press, so a consumer
//! that needs that (the stop record's `was_watched`) captures it itself.
//!
//! ## Ownership
//!
//! [`ViewLedger`] is a cheap handle (`Rc`) on a main-thread table. `stores::Stores` creates one and
//! gives a clone to every store that reads; `publish_view` records into it, once per change, before
//! it calls the stores. The stamp travels into a worker with the request as a plain `u64`; the
//! table itself never leaves the main thread.
//!
//! Entries are not pruned. Deciding that no outstanding read predates an entry is a per-store
//! proof (every store has its own supersede and abandon paths), and until each store's is written
//! down the bounded-by-items-touched table is the safe side to err on.

use crate::viewstate::{ViewAt, ViewEdit};
use std::cell::RefCell;
use std::rc::Rc;

/// Where in the publish order a read was issued: the sequence number of the last edit it can
/// already see. `0` precedes every edit.
pub type Stamp = u64;

#[derive(Clone, Debug)]
struct Entry {
    seq: Stamp,
    at: ViewAt,
    edit: ViewEdit,
}

#[derive(Default)]
struct Table {
    seq: Stamp,
    /// Ascending by `seq`: a replaced entry is removed and the new one appended.
    entries: Vec<Entry>,
}

/// Shared handle on the publish-order table. `Clone` shares the table; `Default` starts a new one.
#[derive(Clone, Default)]
pub struct ViewLedger(Rc<RefCell<Table>>);

/// Do two edits answer the same question about an item, so the newer supersedes the older?
fn same_kind(a: ViewEdit, b: ViewEdit) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
}

impl ViewLedger {
    /// Note one published change and return its sequence number.
    pub fn record(&self, at: &ViewAt, edit: ViewEdit) -> Stamp {
        let mut t = self.0.borrow_mut();
        t.seq += 1;
        let seq = t.seq;
        t.entries.retain(|e| !(e.at == *at && same_kind(e.edit, edit)));
        t.entries.push(Entry { seq, at: at.clone(), edit });
        seq
    }

    /// The stamp for a read issued now.
    pub fn stamp(&self) -> Stamp {
        self.0.borrow().seq
    }

    /// Edits published after `stamp`, oldest first. Empty for a read nothing overtook, which is the
    /// common case and costs one borrow.
    pub fn since(&self, stamp: Stamp) -> Vec<(ViewAt, ViewEdit)> {
        let t = self.0.borrow();
        if t.seq <= stamp {
            return Vec::new();
        }
        t.entries.iter().filter(|e| e.seq > stamp).map(|e| (e.at.clone(), e.edit)).collect()
    }

    /// Make every edit published after `stamp` again on `rows`, which a read stamped `stamp` is
    /// about to place. Returns whether any row changed. A deck removal is not a field of a row, so
    /// only the stores that own a deck act on it (via [`ViewLedger::since`]).
    pub fn patch<'a>(&self, stamp: Stamp, rows: impl IntoIterator<Item = &'a mut crate::pms::PmsMovie>) -> bool {
        let edits = self.since(stamp);
        if edits.is_empty() {
            return false;
        }
        let mut hit = false;
        for m in rows {
            for (at, edit) in &edits {
                if matches!(edit, ViewEdit::Watched(_)) && at.covers(m) {
                    edit.apply(m);
                    hit = true;
                }
            }
        }
        hit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pms::PmsMovie;
    use plx_plex::plex::ServerId;

    fn row(rk: &str) -> PmsMovie {
        let mut m = PmsMovie::default();
        m.sid = ServerId::UNSET;
        m.rk = rk.into();
        m.unwatched = true;
        m.resume_ms = 5;
        m
    }

    #[test]
    fn only_edits_after_the_stamp_are_made_again() {
        let ledger = ViewLedger::default();
        ledger.record(&ViewAt::leaf(ServerId::UNSET, "1"), ViewEdit::Watched(true));
        let stamp = ledger.stamp();
        ledger.record(&ViewAt::leaf(ServerId::UNSET, "2"), ViewEdit::Watched(true));
        let mut rows = [row("1"), row("2")];
        assert!(ledger.patch(stamp, rows.iter_mut()));
        assert!(rows[0].unwatched, "the server already had edit 1 when this read was answered");
        assert!(rows[1].watched && rows[1].resume_ms == 0);
    }

    #[test]
    fn the_newest_edit_on_an_item_wins_and_the_table_does_not_grow() {
        let ledger = ViewLedger::default();
        let at = ViewAt::leaf(ServerId::UNSET, "1");
        for on in [true, false, true, false] {
            ledger.record(&at, ViewEdit::Watched(on));
        }
        assert_eq!(ledger.since(0).len(), 1);
        let mut rows = [row("1")];
        ledger.patch(0, rows.iter_mut());
        assert!(!rows[0].watched, "last press was Unwatched");
    }

    #[test]
    fn edits_on_different_items_replay_in_publish_order() {
        let ledger = ViewLedger::default();
        let show = ViewAt { sid: ServerId::UNSET, rk: "S".into(), scope: crate::viewstate::Scope::Show };
        ledger.record(&show, ViewEdit::Watched(true));
        ledger.record(&ViewAt::leaf(ServerId::UNSET, "E"), ViewEdit::Watched(false));
        let mut e = row("E");
        e.kind = 3;
        e.show_rk = "S".into();
        ledger.patch(0, std::iter::once(&mut e));
        assert!(!e.watched, "the episode was un-ticked after its show was ticked");
    }

    #[test]
    fn a_read_nothing_overtook_is_untouched() {
        let ledger = ViewLedger::default();
        ledger.record(&ViewAt::leaf(ServerId::UNSET, "1"), ViewEdit::Watched(true));
        let mut rows = [row("1")];
        assert!(!ledger.patch(ledger.stamp(), rows.iter_mut()));
        assert!(rows[0].unwatched);
    }
}
