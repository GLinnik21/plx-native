//! The element-keyed pop pool: a card's focus-pop spring belongs to its ELEMENT, not to the cell it
//! happens to stand in.
//!
//! Were the spring the cell's, a card still shrinking (the one focus just left) would stay in its
//! cell, so after an insert or reorder ahead of it the shrink would play out on whichever card now
//! sits there while the real one snapped to rest. The FOCUSED card's spring is carried to its new
//! cell by the landing rule regardless; the pool does the same for the cards letting go.
//!
//! The pool remembers which ELEMENT each moving spring belongs to, and each tick moves a spring
//! after its element to wherever the source now shows it. A walk with no reorder is bit-for-bit
//! the walk by position.
//!
//! A Shelf's pool ([`RowPool`]) is bounded and allocation-free: [`CAP`] entries in fixed arrays, only
//! cards whose spring is off rest are held (an entry settles out), and a lookup hashes at most
//! [`MAX_ROW_ITEMS`] elements, only when a held element is no longer where it was.
//!
//! A Grid's is the let-gos of its [`GridPop`](crate::poster_grid::GridPop): one per card still
//! letting go, each born with the element it was released from, sparse and uncapped (see there). What
//! the thousands of cards of a Library still cannot cost is a scan: a tick hashes ONE element per
//! let-go in flight, and only a let-go whose element is no longer in its cell searches, nearest
//! cell first and no further than [`SEARCH`] from where it was ([`follow_let_gos`]).
//!
//! The DRAW path is element-aware too ([`RowPool::drawn`], [`let_go_cards`]): a frame drawn after
//! the source changed and before the next tick already shows each element at its own spring's
//! scale, so no frame reverses what the tick then corrects. A Shelf's pool hashes the card it
//! draws, at most [`MAX_ROW_ITEMS`] of them. A Grid's frame draws ~40 cards, so it does not: it
//! verifies each let-go's own card once ([`let_go_cards`], one hash per let-go in flight and none
//! when the grid is settled), and only a frame that finds one of them moved resolves every card by
//! its element ([`let_go_drawn`]); every other card is read by its cell.

use std::hash::{Hash, Hasher};

use plx_machine::machine::Host;

use super::CardSource;
use crate::card_row::{CardRow, MAX_ROW_ITEMS};

/// What a card's pop spring is, drawn before the tick has seen a source change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Drawn {
    /// The spring of this cell: the element is the one that was there.
    Cell(usize),
    /// At rest: the cell's spring belongs to an element that is no longer there.
    Rest,
}

/// Moving springs a Shelf's [`RowPool`] holds at once: the focused card, the one shrinking, and a
/// few let-gos of a fast key repeat still on their way to rest.
pub(crate) const CAP: usize = 8;

/// How far from a grid let-go's previous cell its element is looked for: the source of a
/// Library grid has thousands of cards and a tick must not hash them all. One that a landing
/// moved further than this lets go.
pub(crate) const SEARCH: usize = 96;

/// `e`'s identity as a plain number: sections are not generic over the host, so the pool
/// cannot store an `H::Elem`. `DefaultHasher::new()` is unkeyed, so the number is stable
/// within a run; a collision could only mis-route one pop spring for a frame.
pub(crate) fn key<E: Hash>(e: &E) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    e.hash(&mut h);
    h.finish()
}

/// Whether the element whose [`key`] is `k` stands in `cell` of `src`.
fn stands_at<H: Host, S: CardSource<H>>(src: &S, cell: usize, k: u64) -> bool {
    cell < src.len() && key(&src.elem(cell)) == k
}

/// The first of the cells that have their own spring (`0..MAX_ROW_ITEMS`, within the source)
/// showing the element whose [`key`] is `k`.
fn find<H: Host, S: CardSource<H>>(src: &S, k: u64) -> Option<usize> {
    (0..src.len().min(MAX_ROW_ITEMS)).find(|&j| key(&src.elem(j)) == k)
}

/// The cell NEAREST `at` showing the element whose [`key`] is `k`, at any distance: a rule that
/// keeps focus where it is must not depend on how far the cards moved. Work is bounded by the
/// source (one hash per card, nearest first) and is spent only on a tick whose focused element
/// is no longer in its cell, which [`anchor_of`] re-arms at the end of, so a landing pays it once.
pub(crate) fn find_nearest<H: Host, S: CardSource<H>>(src: &S, k: u64, at: usize) -> Option<usize> {
    find_nearest_within::<H, S>(src, k, at, usize::MAX)
}

/// [`find_nearest`] no further than `reach` cells from `at` (`at` itself first): work grows with
/// how far the element went, never with the size of the source, so a Library of thousands pays for
/// the shift of a landing and no more.
pub(crate) fn find_nearest_within<H: Host, S: CardSource<H>>(src: &S, k: u64, at: usize, reach: usize) -> Option<usize> {
    (0..src.len().max(at.saturating_add(1)))
        .take(reach.saturating_add(1))
        .find_map(|d| {
            [at.checked_sub(d), at.checked_add(d).filter(|_| d > 0)].into_iter().flatten().find(|&j| stands_at::<H, S>(src, j, k))
        })
}

/// The element a section had focused at the end of its tick and the cell it stood in: what
/// [`slid`] reads next tick. Kept by the section itself, since a deliberate move re-arms the
/// grid's pop on the NEW cell before the tick runs and the row's own record is the old one.
pub(crate) fn anchor_of<H: Host, S: CardSource<H>>(src: &S, focus: Option<usize>) -> Option<(usize, u64)> {
    focus.filter(|&i| i < src.len()).map(|i| (i, key(&src.elem(i))))
}

/// Where the card an [`anchor_of`] named stands now when a content landing moved it and it is
/// still in the source: `(was, is)`. The one rule behind a Shelf's and a Grid's carry of a focus
/// that changes in the same tick as the cards move.
pub(crate) fn slid<H: Host, S: CardSource<H>>(src: &S, anchor: Option<(usize, u64)>) -> Option<(usize, usize)> {
    let (p, k) = anchor?;
    if stands_at::<H, S>(src, p, k) {
        return None;
    }
    find_nearest::<H, S>(src, k, p).map(|q| (p, q))
}

/// The elements of one [`CardRow`] whose pop springs are off rest, and the cell each is in.
#[derive(Clone, Copy)]
pub(crate) struct RowPool {
    n: usize,
    key: [u64; CAP],
    at: [usize; CAP],
}

impl RowPool {
    pub(crate) const fn new() -> Self {
        Self { n: 0, key: [0; CAP], at: [0; CAP] }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.n
    }

    pub(crate) fn holds(&self, cell: usize) -> bool {
        self.at[..self.n].contains(&cell)
    }

    pub(crate) fn clear(&mut self) {
        self.n = 0;
    }

    /// Before the tick steps `row`: move the spring of every held element that is no longer in
    /// its cell to the cell the source shows it in (its cell goes to rest; an element that
    /// left the source takes its let-go with it). Whether the spring of the element in cell
    /// `focus` was among those carried, so the landing rule must not carry it a second time.
    ///
    /// The focused element pushed past the last cell with its own spring is not found there,
    /// but it did not leave: its spring stays put for the landing rule, which carries it.
    pub(crate) fn follow<H: Host, S: CardSource<H>>(&mut self, row: &mut CardRow, src: &S, focus: Option<usize>) -> bool {
        let mut moved = [(0usize, None::<usize>, crate::Spring::at(1.0)); CAP];
        let mut m = 0;
        for e in 0..self.n {
            let at = self.at[e];
            if stands_at::<H, S>(src, at, self.key[e]) {
                continue;
            }
            let sp = row.cell_spring(at).unwrap_or(crate::Spring::at(1.0));
            moved[m] = (e, find::<H, S>(src, self.key[e]), sp);
            m += 1;
        }
        let beyond = |k: u64| focus.is_some_and(|f| stands_at::<H, S>(src, f, k));
        let mut carried = false;
        for &(e, to, _) in &moved[..m] {
            if to.is_none() && beyond(self.key[e]) {
                continue;
            }
            row.rest(self.at[e]);
        }
        for &(e, to, sp) in &moved[..m] {
            if let Some(to) = to {
                row.put_cell_spring(to, sp);
                self.at[e] = to;
                carried |= focus == Some(to);
            }
        }
        if m > 0 {
            // an element that is gone is dropped (its cell was rested above)
            let mut w = 0;
            for r in 0..self.n {
                if moved[..m].iter().any(|&(e, to, _)| e == r && to.is_none()) {
                    continue;
                }
                self.key[w] = self.key[r];
                self.at[w] = self.at[r];
                w += 1;
            }
            self.n = w;
        }
        carried
    }

    /// After the tick stepped `row`: let settled entries go, hold the focused card first (the
    /// most settled let-go makes room for it in a full pool), then every other card whose
    /// spring is off rest while there is room; the rest fall back to per-position until an
    /// entry frees.
    pub(crate) fn admit<H: Host, S: CardSource<H>>(&mut self, row: &CardRow, src: &S, focus: Option<usize>) {
        let spring = |i: usize| row.cell_spring(i);
        let off_rest = |i: usize| spring(i).is_some_and(|sp| !plx_machine::idle::settled(sp.pos, 1.0, sp.vel));
        let mut w = 0;
        for r in 0..self.n {
            if off_rest(self.at[r]) {
                self.key[w] = self.key[r];
                self.at[w] = self.at[r];
                w += 1;
            }
        }
        self.n = w;
        if let Some(f) = focus.filter(|&f| f < MAX_ROW_ITEMS && f < src.len() && off_rest(f)) {
            let k = key(&src.elem(f));
            let at = self.at[..self.n].iter().position(|&a| a == f).unwrap_or_else(|| {
                if self.n == CAP {
                    // the let-go nearest rest
                    let away = |r: usize| spring(self.at[r]).map_or(0.0, |sp| (sp.pos - 1.0).abs() + 0.1 * sp.vel.abs());
                    let drop = (0..self.n).fold(0, |b, r| if away(r) < away(b) { r } else { b });
                    self.key[drop] = self.key[self.n - 1];
                    self.at[drop] = self.at[self.n - 1];
                    self.n -= 1;
                }
                self.n += 1;
                self.n - 1
            });
            self.key[at] = k;
            self.at[at] = f;
        }
        for i in 0..src.len().min(MAX_ROW_ITEMS) {
            if self.n == CAP {
                break;
            }
            if off_rest(i) && !self.at[..self.n].contains(&i) {
                self.key[self.n] = key(&src.elem(i));
                self.at[self.n] = i;
                self.n += 1;
            }
        }
    }

    /// What card `i` (`focused`: the engine's focus is on it) is drawn with when the source
    /// changed since the last tick: the spring of the cell its element was held in, rest for a
    /// card standing where a held element no longer is. `None`: nothing moved, the rule by
    /// position applies.
    pub(crate) fn drawn<H: Host, S: CardSource<H>>(&self, src: &S, i: usize, focused: bool) -> Option<Drawn> {
        if self.n == 0 || i >= src.len() {
            return None;
        }
        drawn_among((0..self.n).map(|e| (self.at[e], self.key[e])), i, key(&src.elem(i)), focused)
    }
}

/// The one rule a draw between a source change and the next tick follows, for a pool that holds
/// `(cell, key)` pairs: card `i`, whose element's key is `k`, is drawn with the spring of the cell
/// its element was held in, at rest when it stands where a held element no longer is, `None` when
/// nothing moved (the rule by position applies).
fn drawn_among(held: impl Iterator<Item = (usize, u64)>, i: usize, k: u64, focused: bool) -> Option<Drawn> {
    let mut held_here = false;
    for (at, held_key) in held {
        if held_key == k {
            return (at != i).then_some(Drawn::Cell(at));
        }
        held_here |= at == i;
    }
    (held_here && !focused).then_some(Drawn::Rest)
}

/// Before the tick: every let-go follows its element to the cell the source shows it in (nearest
/// the cell it was in, within [`SEARCH`] of it), or is dropped when the element left.
///
/// `carried` is the move the landing rule already found for the card the last tick had focused,
/// `(was, now)` (see [`slid`], which looks for it at any distance): the let-go that card was born
/// with when a key took focus off it between the ticks stands in `was`, since the cell a let-go
/// carries is where the last tick saw its element, and goes to `now` however far that is. (A cell
/// holds one card, and the one the pop stood in is that card.)
///
/// Cost: one element hashed per let-go in flight on an ordinary tick, and for a let-go whose
/// element is no longer in its cell about two more per cell it moved (nearest first), so a landing
/// under a held key pays for the shift of the cards and the let-gos in flight, never for the
/// Library's thousands.
pub(crate) fn follow_let_gos<H: Host, S: CardSource<H>>(
    pop: &mut crate::poster_grid::GridPop,
    src: &S,
    carried: Option<(usize, usize)>,
) {
    pop.follow_let_gos(|c, k| match carried {
        Some((was, now)) if c == was => Some(now),
        _ => find_nearest_within::<H, S>(src, k, c, SEARCH),
    });
}

/// How a frame finds the card each of a [`GridPop`](crate::poster_grid::GridPop)'s let-gos belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LetGoCards {
    /// Every let-go's element stands in the cell the last tick left its let-go in, so a card is the
    /// let-go of its cell, if it has one: no card is hashed.
    ByCell,
    /// The source changed since the last tick (a landing, an insert, a removal) or a press armed
    /// a let-go over cards that had: each card is looked up by its element, as [`RowPool::drawn`] does.
    ByElement,
}

/// Which of the two ways a frame resolves a grid's let-gos ([`LetGoCards`]). Asked once per pass
/// over the cards (the draw, the stops), not once per card: it hashes one element per let-go in
/// flight, and none for a grid with none.
pub(crate) fn let_go_cards<H: Host, S: CardSource<H>>(pop: &crate::poster_grid::GridPop, src: &S) -> LetGoCards {
    if pop.let_gos().all(|c| stands_at::<H, S>(src, c.cell, c.key)) {
        LetGoCards::ByCell
    } else {
        LetGoCards::ByElement
    }
}

/// What card `i` is drawn with when [`let_go_cards`] says the source changed since the last tick, as
/// [`RowPool::drawn`] but for every let-go of a [`GridPop`](crate::poster_grid::GridPop). The focused
/// card's pop follows its element by the landing rule.
pub(crate) fn let_go_drawn<H: Host, S: CardSource<H>>(pop: &crate::poster_grid::GridPop, src: &S, i: usize, focused: bool) -> Option<Drawn> {
    if focused || i >= src.len() {
        return None;
    }
    drawn_among(pop.let_gos().map(|c| (c.cell, c.key)), i, key(&src.elem(i)), false)
}
