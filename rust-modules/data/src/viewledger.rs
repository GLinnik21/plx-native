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
//!   `PmsMovie`s). Every edit whose sequence number is greater than the stamp is made again on
//!   those rows, in sequence order; an edit at or below the stamp is not.
//!
//! ## What a sequence number means
//!
//! An edit's number is NOT only when it was pressed. It is "the last moment a read could still
//! have been answered without it": [`ViewLedger::record`] gives it a number at the press, and
//! [`ViewLedger::landed`] gives it a NEW, higher one when the server has taken the write. The
//! write is queued behind others and then travels, so a read requested after the press can still
//! be answered by the server before the write arrived; with the press-time number alone such a
//! read would be treated as having seen the edit and would undo it. After the landing renumbers
//! the entry, every read requested before the landing replays it and every read requested after
//! it does not (the server has it). A write the server REFUSES is [`ViewLedger::retire`]d, so an
//! edit that did not take never keeps replaying over the server's own state.
//!
//! The guarantee is therefore: a press survives every read that was requested before its write
//! landed, and is never held against a read requested after. Between the press and the landing
//! the entry stands (a server that never answers leaves the edit standing, as `viewstate` says).
//!
//! The read-back worker ([`crate::stores::viewreconcile`]) is the one reader that stamps AND is
//! answered by `ViewFact::Facts` instead of `patch`: it drops a row a newer press covers
//! (`since(stamp)`), and its own Facts edits are never recorded here.
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
//! Entries are dropped by [`ViewLedger::reset`] when the data they describe stops being the
//! current one (sign-out, profile switch, a changed server roster); stamps stay valid across it.
//! Otherwise entries are not pruned. Deciding that no outstanding read predates an entry is a per-store
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

    /// The write for the edit recorded at `seq` has been TAKEN by the server: renumber that entry
    /// to a fresh, higher number, so every read stamped before now (it may have been answered by the
    /// server before the write arrived) still replays it. Does nothing when a newer press on the
    /// same item has replaced the entry meanwhile: that press's own write has not landed, and its
    /// own entry carries it.
    pub fn landed(&self, seq: Stamp) {
        let mut t = self.0.borrow_mut();
        let Some(i) = t.entries.iter().position(|e| e.seq == seq) else { return };
        let mut entry = t.entries.remove(i);
        t.seq += 1;
        entry.seq = t.seq;
        t.entries.push(entry);
    }

    /// The write for the edit recorded at `seq` was REFUSED (or never left): forget the entry, so a
    /// refused edit does not keep replaying over what the server actually says. Does nothing for an
    /// entry a newer press already replaced.
    pub fn retire(&self, seq: Stamp) {
        self.0.borrow_mut().entries.retain(|e| e.seq != seq);
    }

    /// Forget every entry, keeping the sequence so stamps already handed out stay ordered. For the
    /// moments the data the entries describe stops being the current one.
    pub fn reset(&self) {
        let mut t = self.0.borrow_mut();
        t.entries.clear();
        t.seq += 1; // every read in flight is now older than anything recorded after this
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
    /// about to place. A deck removal is not a field of a row, so
    /// only the stores that own a deck act on it (via [`ViewLedger::since`]).
    pub fn patch<'a>(&self, stamp: Stamp, rows: impl IntoIterator<Item = &'a mut crate::pms::PmsMovie>) {
        let edits = self.since(stamp);
        if edits.is_empty() {
            return;
        }
        for m in rows {
            for (at, edit) in &edits {
                if matches!(edit, ViewEdit::Watched(_)) && at.covers(m) {
                    edit.apply(m);
                }
            }
        }
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
        ledger.patch(stamp, rows.iter_mut());
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
        ledger.patch(ledger.stamp(), rows.iter_mut());
        assert!(rows[0].unwatched);
    }

    /// The read was requested after the press but answered before the write reached the server,
    /// so it carries the pre-press body. The landing renumbers the entry; the read still replays it.
    #[test]
    fn a_press_survives_a_read_requested_after_it_and_answered_before_its_write_landed() {
        let ledger = ViewLedger::default();
        let at = ViewAt::leaf(ServerId::UNSET, "1");
        let pressed = ledger.record(&at, ViewEdit::Watched(true));
        let read_stamp = ledger.stamp(); // requested after the press, before the write landed
        assert!(ledger.since(read_stamp).is_empty(), "press-time numbering alone would skip the press");
        ledger.landed(pressed);
        let mut rows = [row("1")];
        ledger.patch(read_stamp, rows.iter_mut());
        assert!(rows[0].watched, "the press must survive that read");
        // A read requested after the landing has the edit server-side and is left alone.
        let later = ledger.stamp();
        let mut rows = [row("1")];
        ledger.patch(later, rows.iter_mut());
        assert!(rows[0].unwatched, "a read requested after the landing is the server's own word");
    }

    #[test]
    fn a_refused_write_stops_replaying() {
        let ledger = ViewLedger::default();
        let seq = ledger.record(&ViewAt::leaf(ServerId::UNSET, "1"), ViewEdit::Watched(true));
        let stamp = ledger.stamp();
        ledger.retire(seq);
        let mut rows = [row("1")];
        ledger.patch(0, rows.iter_mut());
        assert!(rows[0].unwatched, "the server refused it, so its state is the truth");
        assert!(ledger.since(stamp).is_empty());
    }

    #[test]
    fn a_landing_does_not_renumber_an_entry_a_newer_press_replaced() {
        let ledger = ViewLedger::default();
        let at = ViewAt::leaf(ServerId::UNSET, "1");
        let first = ledger.record(&at, ViewEdit::Watched(true));
        let second = ledger.record(&at, ViewEdit::Watched(false));
        let stamp = ledger.stamp();
        ledger.landed(first); // the first write landed; the second has not
        assert!(ledger.since(stamp).is_empty(), "the second press keeps its own number until its write lands");
        ledger.landed(second);
        assert_eq!(ledger.since(stamp).len(), 1);
    }

    #[test]
    fn reset_forgets_every_entry_and_keeps_stamps_ordered() {
        let ledger = ViewLedger::default();
        ledger.record(&ViewAt::leaf(ServerId::UNSET, "1"), ViewEdit::Watched(true));
        let stamp = ledger.stamp();
        ledger.reset();
        assert!(ledger.since(0).is_empty());
        assert!(ledger.stamp() > stamp);
        ledger.record(&ViewAt::leaf(ServerId::UNSET, "2"), ViewEdit::Watched(true));
        assert_eq!(ledger.since(stamp).len(), 1);
    }
}
