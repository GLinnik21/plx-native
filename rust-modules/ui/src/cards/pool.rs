//! The element-keyed pop pool: the alternative to per-position pop springs (shared-card-sections
//! plan, owner decision 2), behind the dev switch `/tmp/plxnative-poppool` for a verdict by feel on
//! the television. **Whichever mode loses is deleted** — this whole file, its call sites in
//! `shelf.rs` / `grid.rs` and the two `card_row` / `poster_grid` accessors go with it.
//!
//! Per position (the default): a pop spring belongs to a CELL. The landing rule already carries
//! the FOCUSED card's spring to its new cell when content reorders, but a card still shrinking
//! (the one focus just left) stays in its cell, so after an insert or reorder ahead of it the
//! shrink plays out on whichever card now sits there while the real one snaps to rest.
//!
//! Element-keyed: the pool remembers which ELEMENT each moving spring belongs to, and each tick
//! moves a spring after its element to wherever the source now shows it. Nothing else changes: a
//! walk with no reorder is bit-for-bit the per-position walk, and with the switch off none of
//! this runs.
//!
//! Bounded and allocation-free: [`CAP`] entries in fixed arrays, only cards whose spring is off
//! rest are held (an entry settles out), a lookup hashes at most [`MAX_ROW_ITEMS`] elements, and
//! only when a held element is no longer where it was.

use std::hash::{Hash, Hasher};

use plx_machine::machine::Host;

use super::CardSource;
use crate::card_row::{CardRow, MAX_ROW_ITEMS};

/// Moving springs held at once: the focused card, the one shrinking, and a few let-gos of a fast
/// key repeat still on their way to rest.
pub(crate) const CAP: usize = 8;

#[cfg(any(test, feature = "devtriggers"))]
thread_local! {
    /// `/tmp/plxnative-poppool` ([`set_pop_pool`]). Thread-local like the other UI-thread dev
    /// switches (`motion::hold_phase_clocks`): the one writer and every reader are the UI thread,
    /// and a process global would leak between tests.
    static POOL_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Key the pop springs of every [`Shelf`](super::Shelf) and [`Grid`](super::Grid) by element
/// (`true`) or by position (`false`, the default). Dev builds only; the shipping feature set has
/// no switch to flip and [`pop_pool`] is the constant `false`.
#[cfg(any(test, feature = "devtriggers"))]
pub fn set_pop_pool(on: bool) {
    POOL_ON.with(|p| p.set(on));
}

/// Is the element-keyed pool on? Always `false` without `devtriggers`.
#[inline]
pub fn pop_pool() -> bool {
    #[cfg(any(test, feature = "devtriggers"))]
    {
        POOL_ON.with(|p| p.get())
    }
    #[cfg(not(any(test, feature = "devtriggers")))]
    {
        false
    }
}

/// `e`'s identity as a plain number: sections are not generic over the host, so the pool cannot
/// store an `H::Elem`. `DefaultHasher::new()` is unkeyed, so the number is stable within a run; a
/// collision could only mis-route one pop spring of a dev build.
fn key<E: Hash>(e: &E) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    e.hash(&mut h);
    h.finish()
}

/// The first of the cells that have their own spring (`0..MAX_ROW_ITEMS`, within the source)
/// showing the element whose [`key`] is `k`.
fn find<H: Host, S: CardSource<H>>(src: &S, k: u64) -> Option<usize> {
    (0..src.len().min(MAX_ROW_ITEMS)).find(|&j| key(&src.elem(j)) == k)
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

    pub(crate) fn clear(&mut self) {
        self.n = 0;
    }

    /// Before the tick steps `row`: move the spring of every held element that is no longer in its
    /// cell to the cell the source shows it in (its cell goes to rest; an element that left the
    /// source takes its let-go with it). Whether the spring of the element in cell `focus` was
    /// among those carried, so the landing rule must not carry it a second time.
    pub(crate) fn follow<H: Host, S: CardSource<H>>(&mut self, row: &mut CardRow, src: &S, focus: Option<usize>) -> bool {
        let mut moved = [(0usize, None::<usize>, crate::Spring::at(1.0)); CAP];
        let mut m = 0;
        for e in 0..self.n {
            let at = self.at[e];
            if at < src.len() && key(&src.elem(at)) == self.key[e] {
                continue;
            }
            let sp = row.cell_spring(at).unwrap_or(crate::Spring::at(1.0));
            moved[m] = (e, find::<H, S>(src, self.key[e]), sp);
            m += 1;
        }
        let mut carried = false;
        for &(e, _, _) in &moved[..m] {
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

    /// After the tick stepped `row`: let settled entries go and hold every card whose spring is
    /// off rest (the focused one first, so a full pool never drops it). A full pool simply holds
    /// no more; those cards fall back to per-position until an entry frees.
    pub(crate) fn admit<H: Host, S: CardSource<H>>(&mut self, row: &CardRow, src: &S, focus: Option<usize>) {
        let off_rest = |i: usize| {
            row.cell_spring(i).is_some_and(|sp| !plx_machine::idle::settled(sp.pos, 1.0, sp.vel))
        };
        let mut w = 0;
        for r in 0..self.n {
            if off_rest(self.at[r]) {
                self.key[w] = self.key[r];
                self.at[w] = self.at[r];
                w += 1;
            }
        }
        self.n = w;
        let first = focus.filter(|&f| f < MAX_ROW_ITEMS);
        for i in first.into_iter().chain(0..src.len().min(MAX_ROW_ITEMS)) {
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
}

/// The shrinking tile of a [`GridPop`](crate::poster_grid::GridPop): the grid's whole pool (it
/// keeps one let-go; the pop itself already follows its element through the landing rule).
#[derive(Clone, Copy)]
pub(crate) struct ShrinkKey(Option<(usize, u64)>);

impl ShrinkKey {
    pub(crate) const fn new() -> Self {
        Self(None)
    }

    /// Before the tick: the let-go follows its element to the cell the source shows it in (or is
    /// dropped when the element left).
    pub(crate) fn follow<H: Host, S: CardSource<H>>(&mut self, pop: &mut crate::poster_grid::GridPop, src: &S) {
        // a let-go armed since the last tick has no key yet: it is still where it started
        let (Some((at, k)), Some(c)) = (self.0, pop.shrinking()) else { return };
        if c != at {
            return;
        }
        if c < src.len() && key(&src.elem(c)) == k {
            return;
        }
        pop.move_shrink((0..src.len()).find(|&j| key(&src.elem(j)) == k));
    }

    /// After the tick: remember which element the running let-go belongs to.
    pub(crate) fn note<H: Host, S: CardSource<H>>(&mut self, pop: &crate::poster_grid::GridPop, src: &S) {
        self.0 = pop.shrinking().filter(|&c| c < src.len()).map(|c| (c, key(&src.elem(c))));
    }
}
