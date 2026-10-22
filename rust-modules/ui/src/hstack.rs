//! **HStack** (and its one-at-a-time sibling [`Flow`]) — one horizontal run of items, spaced the way
//! `UIStackView` spaces them: ONE gap between neighbours, a per-item override for the odd one
//! (`setCustomSpacing`), and the run anchored by its leading edge, its trailing edge or its centre.
//! It is the answer to "what is the gap between these" that a header row, a crumb and a name packer
//! each used to work out for themselves with `x += w + GAP`, and so could drift.
//!
//! **It measures nothing, owns nothing and draws nothing.** A caller hands it widths:
//!
//! - a text run's ADVANCE (text is placed by advance; no horizontal ink query exists), measured by
//!   the caller's own `Measure`. The stack never asks for a width itself, because a replay answers
//!   only the `Width` keys its recording holds and refuses a miss (`rec.rs`): a stack that measured
//!   would add a key to every screen that used it. It places what it is given, so it adds none;
//! - a plate's box, or a glyph's INK via [`Item::glyph`] (the box is then drawn at [`Span::box_x`]);
//! - a spring's value, mid-flight (`Item::new(spring.pos)`): the stack has no state, so the items
//!   after a growing one stand where the value puts them on THIS frame, the way `Column::height`
//!   takes a spring. The motion belongs to whoever owns the spring (`TabStrip`'s `SelMark` is the
//!   row of pills' only glide); the stack just reads the present value.
//!
//! **A glyph is placed by its ink** ([`Item::glyph`]): its bearings come from [`crate::icons::ink_x`], the gap beside
//! it is the token, and [`Span::box_x`] is where the caller draws its box.
//!
//! The gap is a token the caller passes (`theme::space::*`, `page_header::GAP`), never a literal here.
//! The gap between two present neighbours `a` then `b` is `b`'s [`Item::before`] if it has one, else
//! `a`'s [`Item::after`], else the stack's own; an absent item gives and takes neither.
//!
//! **A slot sizes a budget, never air inside the row** (the Library header, #875). A control whose
//! count comes and goes ("Collections · 12") is placed by the text it DRAWS, one gap from its
//! neighbour; the widest case it will ever wear is [`Item::slot`], and it only widens
//! [`HStack::budget_w`], the room the row RESERVES (what the library names may use is measured once,
//! so it never moves when a count does). [`HStack::placed`] and [`HStack::width`] never read it.
//!
//! **Presence is explicit, not read off the width.** An item is either present or [`Item::absent`].
//! An absent item takes no room and no gap, so hiding a control never leaves a double gap where it
//! stood. A present item of width 0 is still present: it keeps its gap on both sides. The reason is
//! the Detail hero's control row (`appkit::hero_layout::row_rect`), which gaps every control
//! whatever its width, so presence is the caller's call: the stack never reads it off a width.
//!
//! An absent item that carries a [`Item::slot`] still counts in [`HStack::budget_w`], at its budget
//! and as if it were there: a control that is not on the row now but may land can reserve its room,
//! so what its neighbour may use does not change when it comes or goes. (The page header does not
//! use this today: it reserves Collections' room through `widest`, the widest-case cluster
//! `PageHeader::new` measures; `Item::absent` is how a pill without an icon and a tab without
//! chapters leave the row, and what `StatusOverlay::row_rects` uses for a part that is not shown and
//! `PageDots` for a block with no dot.) `width` and `placed` skip it, budget and gap both.
//!
//! **A row that must fit its room sheds items by priority** ([`Item::priority`], [`HStack::fit`],
//! [`HStack::kept`]): the lowest priority goes first, the last of equals first, an item with no
//! priority never goes. It drops whole items and measures nothing; a dropped item is absent, so the
//! survivors close up around it, re-anchored by [`Align`]. It is for short rows (at most 64 items).

use crate::icons::{self, Icon};

/// One item in the run: how wide it draws, whether it is there, what it may reserve and the gap
/// after it if that differs from the stack's. Built from the width the caller measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Item {
    /// What it DRAWS: a text run's advance, a plate's width, a spring's value. Taken as given.
    w: f32,
    /// Is it on the row? An absent item takes no room and no gap.
    present: bool,
    /// The widest this item will ever draw: it sizes [`HStack::budget_w`] only, never a placement.
    budget: Option<f32>,
    /// The gap after this item instead of the stack's own (`setCustomSpacing`).
    after: Option<f32>,
    /// The gap before this item, which wins over the neighbour's `after` and the stack's own: see
    /// [`Item::before`].
    before: Option<f32>,
    /// Drop order for [`HStack::fit`]; `None` is never dropped. See the fit section.
    priority: Option<u8>,
    /// A glyph's empty margin left of its ink inside its box (0 for anything else): see
    /// [`Item::glyph`].
    lead: f32,
}

impl Item {
    /// A present item `w` wide. A `w` of 0 is still present, and keeps its gap.
    pub const fn new(w: f32) -> Self {
        Self { w: w, present: true, budget: None, after: None, before: None, priority: None, lead: 0.0 }
    }

    /// An item that is not on the row: no width, no gap. Give it [`Self::slot`] and it still
    /// reserves its room in [`HStack::budget_w`].
    pub const fn absent() -> Self {
        Self { w: 0.0, present: false, budget: None, after: None, before: None, priority: None, lead: 0.0 }
    }

    /// The widest case this item will ever draw, which [`HStack::budget_w`] reserves (the larger of
    /// this and what it draws now). A slot never moves anything: it is budget, not air.
    #[must_use]
    pub const fn slot(mut self, budget: f32) -> Self {
        self.budget = Some(budget);
        self
    }

    /// The gap after this item, in place of the stack's own: a read-out that wants more room before
    /// the control after it, a glyph whose neighbour has a plate. It is the gap to the next item
    /// that is on the row; the last item has none.
    #[must_use]
    pub const fn after(mut self, gap: f32) -> Self {
        self.after = Some(gap);
        self
    }
}

// ---- Glyphs placed by their INK (lane ui-1) ----
impl Item {
    /// A glyph drawn in a `box_w`-wide box, placed by its INK: it is `icon`'s ink width wide
    /// ([`crate::icons::ink_x`] times `box_w`), so the gap beside it is the gap the eye reads, not
    /// the gap to an empty margin. The stack stands the ink; the caller draws the box at
    /// [`Span::box_x`] (the ink's x less the margin the asset leaves on its left).
    pub fn glyph(icon: Icon, box_w: f32) -> Self {
        let (l, r) = icons::ink_x(icon);
        Self { w: (r - l) * box_w, lead: l * box_w, ..Self::new(0.0) }
    }
}

/// Where a run is anchored, as an x.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    /// The first item's leading edge stands at x; the run grows to the right.
    Leading(f32),
    /// The last item's trailing edge stands at x; the run grows to the left, so the items after a
    /// one that changes width stand still.
    Trailing(f32),
    /// The run's middle stands at x.
    Centre(f32),
}

/// Where one present item stands: the span it DRAWS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    /// Its index in the run (absent items leave gaps in the numbering).
    pub i: usize,
    /// Its leading edge.
    pub x: f32,
    /// Its width: what the caller said it draws.
    pub w: f32,
    /// Where to draw the item's BOX: `x` less the empty margin a [`Item::glyph`] leaves left of its
    /// ink. Equal to `x` for every other item.
    pub box_x: f32,
}

/// A run of [`Item`]s with one gap between neighbours, anchored by [`Align`]. It borrows its items
/// (a slice of [`Item`]s, or a slice of widths: [`HStack::widths`]), holds no state and allocates
/// nothing: build it where it is read, as often as that is.
#[derive(Clone, Copy, Debug)]
pub struct HStack<'a> {
    items: Items<'a>,
    gap: f32,
    align: Align,
    /// Which items [`HStack::fit`] kept ([`Kept::ALL`] until [`HStack::kept`] narrows it).
    kept: Kept,
}

/// Where a stack reads its items from: a slice of [`Item`]s, or (see "Runs of plain widths") a slice
/// of widths, each read as [`Item::new`].
#[derive(Clone, Copy, Debug)]
enum Items<'a> {
    Slice(&'a [Item]),
    Widths(&'a [f32]),
}

impl Items<'_> {
    const fn len(&self) -> usize {
        match self {
            Items::Slice(s) => s.len(),
            Items::Widths(w) => w.len(),
        }
    }

    const fn get(&self, i: usize) -> Item {
        match self {
            Items::Slice(s) => s[i],
            Items::Widths(w) => Item::new(w[i]),
        }
    }
}

impl<'a> HStack<'a> {
    /// A run of `items`, `gap` apart (an item's [`Item::after`] overrides it for that one), anchored
    /// by `align`.
    pub const fn new(items: &'a [Item], gap: f32, align: Align) -> Self {
        Self { items: Items::Slice(items), gap, align, kept: Kept::ALL }
    }

    /// The gap between neighbours `a` then `b`: `b`'s [`Item::before`], else `a`'s [`Item::after`],
    /// else the stack's own.
    const fn gap_between(&self, a: &Item, b: &Item) -> f32 {
        gap_between(self.gap, a, b)
    }

    /// Item `i` as this run sees it: itself, or [`Item::absent`] once [`Self::fit`] dropped it.
    const fn live(&self, i: usize) -> Item {
        if self.kept.has(i) { self.items.get(i) } else { Item::absent() }
    }

    /// The sum of `size` over the items that count (`Some`), plus one gap between each neighbouring
    /// pair of them. The widths are added first and the gaps second, so a caller that summed a row
    /// as "runs, then gaps" gets the same float back.
    ///
    /// `budget` picks what counts: `false` the drawn run ([`Self::width`]), `true` the reserved one
    /// ([`Self::budget_w`]). A flag, not a closure, so [`Self::width`] can be a `const fn`.
    const fn sum(&self, budget: bool) -> f32 {
        let (mut runs, mut gaps) = (0.0, 0.0);
        let mut before: Option<Item> = None;
        let mut i = 0;
        while i < self.items.len() {
            let item = self.live(i);
            i += 1;
            let size = if budget {
                match (item.present, item.budget) {
                    (true, Some(b)) => Some(if item.w > b { item.w } else { b }),
                    (true, None) => Some(item.w),
                    (false, b) => b,
                }
            } else if item.present {
                Some(item.w)
            } else {
                None
            };
            let Some(w) = size else { continue };
            runs += w;
            if let Some(prev) = before {
                gaps += self.gap_between(&prev, &item);
            }
            before = Some(item);
        }
        runs + gaps
    }

    /// The run's width as DRAWN: every present item's width and one gap between present
    /// neighbours. An absent item adds neither; a present item of width 0 adds its gaps.
    pub const fn width(&self) -> f32 {
        self.sum(false)
    }

    /// What the run RESERVES: [`Self::width`] with every item at the larger of what it draws and
    /// its [`Item::slot`], and an absent item that has a slot counted at its slot, gap included.
    /// This sizes the room a neighbour may use; it is never where anything stands.
    pub fn budget_w(&self) -> f32 {
        self.sum(true)
    }

    /// Where the first present item's leading edge stands.
    fn start(&self) -> f32 {
        match self.align {
            Align::Leading(x) => x,
            Align::Trailing(x) => x - self.width(),
            Align::Centre(x) => x - self.width() * 0.5,
        }
    }

    /// **THE placement**: every present item, in order, with the span it draws. One pass, no
    /// allocation; absent items are skipped. Draw, focus and hit-test read this, so they cannot
    /// disagree. An item's slot is not read: it never changes where anything stands.
    pub fn placed(&self) -> impl Iterator<Item = Span> + 'a {
        let items = self.items;
        let kept = self.kept;
        let mut flow = Flow::new(self.start(), self.gap);
        (0..items.len()).filter_map(move |i| {
            let item = items.get(i);
            if !kept.has(i) {
                return None;
            }
            flow.push(item).map(|span| Span { i, ..span })
        })
    }

    /// Item `i`'s drawn `(x, w)`, `None` when it is absent or past the end: what a `TabStrip`'s span
    /// closure and a hit test ask for.
    pub fn span(&self, i: usize) -> Option<(f32, f32)> {
        self.placed().find(|placed| placed.i == i).map(|placed| (placed.x, placed.w))
    }
}

// ---- fit: drop the least important items until the run fits its room -----------------------------

impl Item {
    /// How much this item matters to [`HStack::fit`]: the LOWER the number, the sooner it goes. An
    /// item with no priority is never dropped.
    #[must_use]
    pub const fn priority(mut self, p: u8) -> Self {
        self.priority = Some(p);
        self
    }
}

/// Which items of a run survived [`HStack::fit`]: a small `Copy` set, indexed like the run. Items past the 64th are always kept (and never dropped by `fit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kept(u64);

impl Kept {
    /// Every item kept: what a stack holds until [`HStack::kept`] narrows it.
    pub const ALL: Self = Self(u64::MAX);

    /// Is item `i` kept?
    pub const fn has(self, i: usize) -> bool {
        i >= u64::BITS as usize || self.0 & (1 << i) != 0
    }
}

impl<'a> HStack<'a> {
    /// **Drop items until the run's [`Self::width`] is at most `room`.** Each step drops the present,
    /// still-kept item with the LOWEST [`Item::priority`]; among equal priorities the LAST one in the
    /// run goes first, so a row whose items all share a priority sheds from its tail, and a row
    /// listed in order of importance (equal priorities) keeps the longest prefix that fits. An item
    /// with no priority is never dropped: if only those remain the run may still be wider than
    /// `room`. An item is dropped whole (nothing is shrunk) and takes its gap with it, because a
    /// dropped item is absent. Absent items are never dropped: they cost nothing already. A `room`
    /// at or below 0 drops everything droppable.
    ///
    /// It measures nothing: the widths are the ones the caller gave. Read the result back with
    /// [`Self::kept`]: a dropped item is absent, so the survivors close up around it, re-anchored by
    /// [`Align`].
    ///
    /// **For short rows only (at most 64 items):** the result is a 64-bit set, and an item past the
    /// 64th is neither considered for dropping nor ever dropped.
    pub fn fit(&self, room: f32) -> Kept {
        debug_assert!(self.items.len() <= u64::BITS as usize, "fit is for short rows (<= 64 items)");
        let mut kept = self.kept;
        loop {
            if (HStack { kept, ..*self }).width() <= room {
                return kept;
            }
            let mut victim: Option<(usize, u8)> = None;
            for i in 0..self.items.len().min(u64::BITS as usize) {
                let item = self.items.get(i);
                let Some(p) = item.priority else { continue };
                if item.present && kept.has(i) && victim.map_or(true, |(_, low)| p <= low) {
                    victim = Some((i, p));
                }
            }
            let Some((i, _)) = victim else { return kept };
            kept.0 &= !(1 << i);
        }
    }

    /// This run with only the items `kept` names: the rest are absent, so [`Self::placed`],
    /// [`Self::width`] and [`Self::span`] see the survivors alone (an item keeps its index).
    #[must_use]
    pub const fn kept(&self, kept: Kept) -> HStack<'a> {
        HStack { kept, ..*self }
    }
}

// ---- Flow: a leading-anchored run placed one item at a time (lane hs-final) ----

/// **A run placed as it is painted**: the leading-anchored half of [`HStack::placed`], for a run
/// whose items are not in a slice, because their count is the data's (a rating row's cells) or
/// because each is painted, and so measured, as it is placed (a heading's runs). Same rule as the
/// stack's: one gap between neighbours, an [`Item::after`] in its place, an absent item no room and
/// no gap. It holds only the pen, allocates nothing and measures nothing. A caller that knows all
/// its widths up front builds an [`HStack`] instead; [`HStack::placed`] is itself a `Flow`.
#[derive(Clone, Copy, Debug)]
pub struct Flow {
    x: f32,
    gap: f32,
    /// The last present item: its width and its own `after`. The pen moves by the width and the gap
    /// to the next item, which only that item's `before` can settle.
    last: Option<(f32, Option<f32>)>,
    /// The trailing edge of the last present item.
    edge: f32,
    /// Counts the items pushed, present or not: the next [`Span::i`].
    next: usize,
}

impl Flow {
    /// A flow whose first item's leading edge stands at `x`, `gap` between neighbours.
    pub const fn new(x: f32, gap: f32) -> Self {
        Self { x, gap, last: None, edge: x, next: 0 }
    }

    /// Place the next item: the span it draws, `None` (and no room taken) when it is absent.
    pub fn push(&mut self, item: Item) -> Option<Span> {
        let x = self.x_for(&item);
        self.place_item(item, x)
    }

    /// Where `item`'s leading edge stands: the pen moved by the last item's width and the gap
    /// between it and `item`.
    fn x_for(&self, item: &Item) -> f32 {
        match self.last {
            None => self.x,
            Some((w, after)) => {
                let prev = Item { w, after, ..Item::new(0.0) };
                self.x + (w + gap_between(self.gap, &prev, item))
            }
        }
    }

    /// Place the next item, handing `make` the x its leading edge will stand at (the pen after the
    /// gap) BEFORE the item exists, for a caller that paints the run to learn its width. `make`
    /// runs for an absent item too, at the pen's current x; its item then takes no room.
    ///
    /// **An item placed this way has no [`Item::before`]** (debug-asserted): the pen is fixed before
    /// the item exists, so a gap that depends on it cannot be honoured. Use [`Self::push`].
    pub fn place(&mut self, make: impl FnOnce(f32) -> Item) -> Option<Span> {
        let x = self.next_x();
        let item = make(x);
        debug_assert!(item.before.is_none(), "Flow::place cannot honour Item::before; use push");
        self.place_item(item, x)
    }

    /// Place `item` with the gap it asks for: [`Item::before`], else the last item's `after`, else
    /// the flow's gap.
    fn place_item(&mut self, item: Item, x: f32) -> Option<Span> {
        let i = self.next;
        self.next += 1;
        if !item.present {
            return None;
        }
        self.x = x;
        self.last = Some((item.w, item.after));
        self.edge = x + item.w;
        Some(Span { i, x, w: item.w, box_x: x - item.lead })
    }

    /// Where the next present item will stand: the pen after the gap (the leading `x` while none is
    /// placed). A caller that places items only while they fit its room asks it before it commits.
    /// It assumes the next item has no [`Item::before`].
    pub fn next_x(&self) -> f32 {
        self.x_for(&Item::new(0.0))
    }

    /// The trailing edge of the last present item (the leading `x` while none is): where the run ends.
    pub const fn end(&self) -> f32 {
        self.edge
    }
}

// ---- before: the gap ahead of an item (lane vs-p0) ----

impl Item {
    /// The gap BEFORE this item, in place of the previous item's [`Item::after`] and the stack's own
    /// (CSS `margin-top`; the vertical precedent is `Column::gap_before`). The gap between present
    /// neighbours `a` then `b` is `b.before`, else `a.after`, else the stack's gap. It is the gap to
    /// the previous item that is on the row, so it means nothing for the first, and an absent item
    /// neither gives nor takes one: a stack whose optional middle item is absent keeps the `before`
    /// of the item after it, measured from the item before that.
    #[must_use]
    pub const fn before(mut self, gap: f32) -> Self {
        self.before = Some(gap);
        self
    }
}

/// The gap between present neighbours `a` then `b` in a run whose own gap is `gap`.
const fn gap_between(gap: f32, a: &Item, b: &Item) -> f32 {
    match (b.before, a.after) {
        (Some(g), _) | (None, Some(g)) => g,
        (None, None) => gap,
    }
}

// ---- Runs of plain widths (lane ui-2) ----
impl<'a> HStack<'a> {
    /// A run of `widths`, each an [`Item::new`] (present, no slot, no gap of its own), `gap` apart:
    /// a strip of labels or a row of pills a caller measured into a slice, read in place with no
    /// [`Item`] array built beside it.
    pub const fn widths(widths: &'a [f32], gap: f32, align: Align) -> Self {
        Self { items: Items::Widths(widths), gap, align, kept: Kept::ALL }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAP: f32 = 50.0;

    /// Every placed item's `(index, x, w)`.
    fn at(stack: &HStack<'_>) -> Vec<(usize, f32, f32)> {
        stack.placed().map(|p| (p.i, p.x, p.w)).collect()
    }

    /// The air between each neighbouring pair of placed items.
    fn gaps(stack: &HStack<'_>) -> Vec<f32> {
        let placed: Vec<Span> = stack.placed().collect();
        placed.windows(2).map(|pair| pair[1].x - (pair[0].x + pair[0].w)).collect()
    }

    fn run(widths: &[f32]) -> Vec<Item> {
        widths.iter().map(|&w| Item::new(w)).collect()
    }

    /// **Equal gaps, whatever the anchor.** Every neighbouring pair stands one gap apart, the run is
    /// as wide as its items and gaps say, and the anchor puts the right edge where it was asked to.
    #[test]
    fn present_items_stand_one_gap_apart_for_every_align() {
        let items = run(&[120.0, 33.5, 80.0, 7.25]);
        let sum = 120.0 + 33.5 + 80.0 + 7.25 + 3.0 * GAP;
        for align in [Align::Leading(96.0), Align::Trailing(1771.0), Align::Centre(960.0)] {
            let stack = HStack::new(&items, GAP, align);
            assert_eq!(stack.width(), sum);
            assert_eq!(gaps(&stack), [GAP; 3], "{align:?}");
            let placed: Vec<Span> = stack.placed().collect();
            let (first, last) = (placed[0], placed[3]);
            match align {
                Align::Leading(x) => assert_eq!(first.x, x),
                Align::Trailing(x) => assert_eq!(last.x + last.w, x),
                Align::Centre(x) => assert!(((first.x + last.x + last.w) * 0.5 - x).abs() < 1e-3),
            }
        }
    }

    /// `setCustomSpacing`: an item's `after` replaces the stack's gap after that item and no other.
    #[test]
    fn after_overrides_the_gap_after_that_one_item() {
        let items = [Item::new(10.0), Item::new(20.0).after(64.0), Item::new(30.0), Item::new(40.0)];
        let stack = HStack::new(&items, GAP, Align::Leading(0.0));
        assert_eq!(gaps(&stack), [GAP, 64.0, GAP]);
        assert_eq!(stack.width(), 10.0 + 20.0 + 30.0 + 40.0 + GAP + 64.0 + GAP);
        // The last item has no gap after it, so its override changes nothing.
        let end = [Item::new(10.0), Item::new(20.0).after(64.0)];
        assert_eq!(HStack::new(&end, GAP, Align::Leading(0.0)).width(), 10.0 + GAP + 20.0);
    }

    /// An absent item takes no room and no gap: hiding the middle one leaves ONE gap where there
    /// were two, never a double gap, wherever it stood in the run.
    #[test]
    fn an_absent_item_takes_no_gap() {
        for hidden in 0..4 {
            let mut items = run(&[10.0, 20.0, 30.0, 40.0]);
            items[hidden] = Item::absent();
            let stack = HStack::new(&items, GAP, Align::Leading(0.0));
            let live: Vec<f32> = [10.0, 20.0, 30.0, 40.0].iter().enumerate().filter(|(i, _)| *i != hidden).map(|(_, w)| *w).collect();
            assert_eq!(stack.width(), live.iter().sum::<f32>() + 2.0 * GAP, "hiding item {hidden}");
            assert_eq!(gaps(&stack), [GAP; 2], "hiding item {hidden}: no double gap");
            assert!(stack.span(hidden).is_none(), "an absent item has no span");
            assert!(at(&stack).iter().all(|(i, ..)| *i != hidden));
        }
        let none = [Item::absent(), Item::absent()];
        assert_eq!(HStack::new(&none, GAP, Align::Trailing(100.0)).width(), 0.0);
        assert_eq!(HStack::new(&[], GAP, Align::Centre(100.0)).placed().count(), 0);
    }

    /// An absent item's own `after` is not read by `width` or `placed`: it is not there to be
    /// followed by a gap. (Nor by `budget_w` while it carries no slot, as here; a SLOTTED absent item
    /// counts in `budget_w` as if it were there, so its `after` is read there when a counted item
    /// follows.)
    #[test]
    fn an_absent_item_leaves_its_neighbours_gap_alone() {
        let items = [Item::new(10.0).after(7.0), Item::absent().after(900.0), Item::new(30.0)];
        let stack = HStack::new(&items, GAP, Align::Leading(0.0));
        assert_eq!(gaps(&stack), [7.0], "the gap after the PRESENT neighbour");
    }

    /// A present item whose width is 0 is still present: it keeps a gap on each side, the way the
    /// Detail row's `row_rect` puts a gap after every control. (A rule that read `w <= 0` as absent
    /// would pop that gap in the frame a width crosses 0.)
    #[test]
    fn a_zero_width_present_item_keeps_its_gap() {
        let items = run(&[10.0, 0.0, 30.0]);
        let stack = HStack::new(&items, GAP, Align::Leading(0.0));
        assert_eq!(stack.width(), 10.0 + 30.0 + 2.0 * GAP);
        assert_eq!(at(&stack), [(0, 0.0, 10.0), (1, 10.0 + GAP, 0.0), (2, 10.0 + GAP + GAP, 30.0)]);
        assert_eq!(stack.span(1), Some((10.0 + GAP, 0.0)));
        // ...and the same width made absent takes the gap with it.
        let gone = [Item::new(10.0), Item::absent(), Item::new(30.0)];
        assert_eq!(HStack::new(&gone, GAP, Align::Leading(0.0)).width(), 10.0 + 30.0 + GAP);
    }

    /// **A slot never places.** Placement is identical with and without the budgets; only
    /// `budget_w` grows by what the slots add, and `width` does not.
    #[test]
    fn a_slot_sizes_the_budget_and_never_the_placement() {
        let bare = [Item::new(100.0), Item::new(60.0), Item::new(40.0)];
        let slotted = [Item::new(100.0), Item::new(60.0).slot(95.0), Item::new(40.0).slot(10.0)];
        for align in [Align::Leading(96.0), Align::Trailing(1771.0), Align::Centre(960.0)] {
            let (a, b) = (HStack::new(&bare, GAP, align), HStack::new(&slotted, GAP, align));
            assert_eq!(at(&a), at(&b), "{align:?}: the slots move nothing");
            assert_eq!(a.width(), b.width());
            assert_eq!(a.budget_w(), a.width(), "no slot: the budget is the width");
        }
        let stack = HStack::new(&slotted, GAP, Align::Leading(0.0));
        assert_eq!(stack.budget_w(), 100.0 + 95.0 + 40.0 + 2.0 * GAP, "a slot narrower than the draw reserves the draw");
        assert_eq!(stack.budget_w() - stack.width(), 35.0);
    }

    /// **An absent item that has a slot can still reserve its room**: its budget AND the gaps around
    /// it, in `budget_w` only. `width` and `placed` skip both. (The page header does not use this
    /// today: it reserves Collections' room through `PageHeader::new`'s `widest`. This is what a row
    /// whose control comes and goes could lean on instead.)
    #[test]
    fn an_absent_budgeted_item_counts_its_budget_and_gap_in_the_budget_only() {
        let there = [Item::new(70.0).slot(70.0), Item::new(300.0).slot(310.0), Item::new(90.0)];
        let away = [Item::new(70.0).slot(70.0), Item::absent().slot(310.0), Item::new(90.0)];
        let (there, away) = (HStack::new(&there, GAP, Align::Trailing(1771.0)), HStack::new(&away, GAP, Align::Trailing(1771.0)));
        assert_eq!(away.budget_w(), there.budget_w(), "absent or not, the room reserved is the same");
        assert_eq!(away.budget_w(), 70.0 + 310.0 + 90.0 + 2.0 * GAP);
        assert_eq!(away.width(), 70.0 + 90.0 + GAP, "the live run is two items and ONE gap");
        assert_eq!(at(&away), [(0, 1771.0 - away.width(), 70.0), (2, 1771.0 - 90.0, 90.0)]);
        assert!(away.span(1).is_none());
        // An absent item with no slot reserves nothing.
        let plain = [Item::new(70.0), Item::absent(), Item::new(90.0)];
        assert_eq!(HStack::new(&plain, GAP, Align::Leading(0.0)).budget_w(), 70.0 + 90.0 + GAP);
    }

    /// A spring-valued width mid-animation places the later items by that value, this frame:
    /// no state, so no lag and no ghost of the previous one.
    #[test]
    fn a_spring_valued_width_places_the_later_items_by_its_current_value() {
        for pos in [0.0, 12.5, 61.0, 211.0] {
            let items = [Item::new(pos), Item::new(61.0), Item::new(61.0)];
            let stack = HStack::new(&items, GAP, Align::Leading(96.0));
            assert_eq!(stack.span(1), Some((96.0 + pos + GAP, 61.0)));
            assert_eq!(stack.span(2), Some((96.0 + pos + GAP + 61.0 + GAP, 61.0)));
            let trailing = HStack::new(&items, GAP, Align::Trailing(1000.0));
            assert_eq!(trailing.span(2).map(|(x, w)| x + w), Some(1000.0), "a trailing run keeps its far end still");
        }
    }

    /// `span(i)` has no span for an absent item or an index past the end (it is `placed()`'s own
    /// span for every present one, by definition).
    #[test]
    fn span_is_none_for_an_absent_item_and_past_the_end() {
        let mut items = run(&[10.0, 20.0, 30.0, 40.0]);
        items[2] = Item::absent();
        for align in [Align::Leading(5.0), Align::Trailing(500.0), Align::Centre(250.0)] {
            let stack = HStack::new(&items, GAP, align);
            assert_eq!(stack.span(2), None);
            assert_eq!(stack.span(items.len()), None);
        }
    }

    /// The widths are summed before the gaps, in item order: the float a header row's "sum of runs
    /// plus sum of gaps" gives. That is bit-identical to a hand sum only when the hand sum also adds
    /// runs before gaps, or when every measure answers whole or half pixels, where float addition is
    /// exact in any order (all the product measures do today: `TTF_SizeUTF8` ints, `unmeasured_width`
    /// bytes * size * 0.5, the replay tables, `Fixed` 8 * len). The two sites whose order changed
    /// onto the stack, `pack_names`' `with_more` and the crumb's root, are fine for that reason.
    #[test]
    fn width_adds_the_runs_first_and_the_gaps_second() {
        let widths = [123.40625, 0.1, 77.7, 3.3, 1e-3];
        let items = run(&widths);
        let want: f32 = widths.iter().sum::<f32>() + 4.0 * GAP;
        assert_eq!(HStack::new(&items, GAP, Align::Leading(0.0)).width(), want);
    }

    /// **fit drops the lowest priority first, the later one on a tie, and keeps a stack's
    /// placement.** Equal priorities shed from the tail (the longest prefix that fits).
    #[test]
    fn fit_sheds_the_tail_of_equal_priorities_and_the_lowest_first() {
        let items: Vec<Item> = [100.0, 60.0, 40.0].iter().map(|&w| Item::new(w).priority(1)).collect();
        let stack = HStack::new(&items, GAP, Align::Leading(0.0));
        // 100 + 50 + 60 + 50 + 40 = 300.
        assert_eq!(stack.fit(300.0), Kept::ALL);
        assert_eq!(stack.kept(stack.fit(299.0)).width(), 210.0, "the last item goes alone");
        let two = stack.fit(299.0);
        assert!(two.has(0) && two.has(1) && !two.has(2));
        let one = stack.fit(209.0);
        assert!(one.has(0) && !one.has(1) && !one.has(2));
        let none = stack.fit(99.0);
        assert!((0..3).all(|i| !none.has(i)));
        assert_eq!(stack.kept(none).placed().count(), 0);
        // A lower priority goes before a later item of higher priority.
        let mixed = [Item::new(100.0).priority(2), Item::new(60.0).priority(1), Item::new(40.0).priority(2)];
        let stack = HStack::new(&mixed, GAP, Align::Leading(0.0));
        let kept = stack.fit(250.0);
        assert!(kept.has(0) && !kept.has(1) && kept.has(2), "the middle one has the lowest priority");
        assert_eq!(gaps(&stack.kept(kept)), [GAP], "a dropped item takes its gap with it");
    }

    /// An item with no priority is never dropped; an exact fit is kept; a room at or below zero
    /// drops everything droppable.
    #[test]
    fn fit_never_drops_an_unprioritised_item_and_keeps_an_exact_fit() {
        let items = [Item::new(100.0), Item::new(50.0).priority(1)];
        let stack = HStack::new(&items, 20.0, Align::Leading(10.0));
        assert_eq!(stack.fit(170.0), Kept::ALL, "100 + 20 + 50 fits exactly");
        let tight = stack.fit(169.0);
        assert!(tight.has(0) && !tight.has(1));
        let never = stack.fit(0.0);
        assert!(never.has(0) && !never.has(1), "the unprioritised item stays however small the room");
        assert_eq!(stack.kept(never).span(0), Some((10.0, 100.0)));
        let all = [Item::new(10.0).priority(1)];
        let stack = HStack::new(&all, GAP, Align::Leading(0.0));
        assert!(!stack.fit(-5.0).has(0));
        assert!(!stack.fit(0.0).has(0), "an empty run is 0 wide, but a 10 wide item is not <= 0");
        assert_eq!(HStack::new(&[], GAP, Align::Leading(0.0)).fit(964.0), Kept::ALL);
    }

    /// `kept` narrows `budget_w` too: a dropped slotted item reserves nothing.
    #[test]
    fn a_dropped_item_reserves_no_budget() {
        let items = [Item::new(10.0), Item::new(20.0).slot(90.0).priority(1)];
        let stack = HStack::new(&items, GAP, Align::Leading(0.0));
        assert_eq!(stack.budget_w(), 10.0 + GAP + 90.0);
        assert_eq!(stack.kept(stack.fit(15.0)).budget_w(), 10.0);
    }

    /// **A glyph is placed by its ink**: the visible gap beside it is the stack's gap whatever
    /// margin its box leaves, and the box is drawn `lead` left of the ink. A glyph with no margin
    /// (`ink_x` default) places box and ink together.
    #[test]
    fn a_glyph_is_placed_by_its_ink_and_drawn_by_its_box() {
        const BOX: f32 = 24.0;
        let (l, r) = icons::ink_x(Icon::Chevron);
        let items = [Item::new(100.0), Item::glyph(Icon::Chevron, BOX), Item::new(40.0)];
        for align in [Align::Leading(96.0), Align::Trailing(1771.0), Align::Centre(960.0)] {
            let stack = HStack::new(&items, GAP, align);
            assert_eq!(gaps(&stack), [GAP; 2], "{align:?}: the INK is a gap from each neighbour");
            let glyph = stack.placed().nth(1).unwrap();
            assert!((glyph.w - (r - l) * BOX).abs() < 1e-4, "it is as wide as its ink");
            assert!((glyph.x - glyph.box_x - l * BOX).abs() < 1e-4, "the box starts the left margin before the ink");
            let first = stack.placed().next().unwrap();
            assert_eq!(first.box_x, first.x, "a plain item's box is its span");
        }
        // The fold pair has bearings now (they were the (0, 1) default, i.e. ink = box).
        let (l, r) = icons::ink_x(Icon::ChevronDown);
        assert!(l > 0.0 && r < 1.0);
        let none = Item::glyph(Icon::Check, BOX);
        let s = HStack::new(std::slice::from_ref(&none), GAP, Align::Leading(10.0)).placed().next().unwrap();
        assert_eq!((s.x, s.w, s.box_x), (10.0, BOX, 10.0), "an unmeasured mark's ink fills its box");
    }

    /// **A run of plain widths stands where the same widths as items do** — placements, width and
    /// budget alike, and `fit`'s survivors — so a strip needs no `Item` array.
    #[test]
    fn a_run_of_widths_places_exactly_as_the_slice_does() {
        let widths = [120.0, 33.5, 7.25, 0.0, 80.0];
        let items = run(&widths);
        for align in [Align::Leading(96.0), Align::Trailing(1771.0), Align::Centre(960.0)] {
            let slice = HStack::new(&items, GAP, align);
            let by_width = HStack::widths(&widths, GAP, align);
            assert_eq!(at(&slice), at(&by_width));
            assert_eq!(slice.width(), by_width.width());
            assert_eq!(slice.budget_w(), by_width.budget_w());
            assert_eq!(slice.fit(300.0), by_width.fit(300.0));
        }
        assert_eq!(HStack::widths(&[], GAP, Align::Leading(0.0)).width(), 0.0);
    }

    /// **A `Flow` is the leading-anchored stack, one item at a time**: it places exactly where
    /// [`HStack::placed`] does (which is built on it), skips an absent item without a gap, honours
    /// an item's own gap, hands `place` the x before the item exists, and reports the pen.
    #[test]
    fn a_flow_places_one_item_at_a_time_where_the_stack_would() {
        let items = [Item::new(30.0), Item::absent(), Item::new(20.0).after(7.0), Item::new(10.0)];
        let stack = HStack::new(&items, GAP, Align::Leading(100.0));
        let mut flow = Flow::new(100.0, GAP);
        assert_eq!(flow.next_x(), 100.0);
        let spans: Vec<Span> = items.iter().filter_map(|&item| flow.push(item)).collect();
        assert_eq!(spans.iter().map(|s| (s.x, s.w)).collect::<Vec<_>>(), at(&stack).iter().map(|&(_, x, w)| (x, w)).collect::<Vec<_>>());
        assert_eq!(flow.end(), 100.0 + stack.width());
        assert_eq!(flow.next_x(), flow.end() + GAP);
        // `place` is told the x before it has the item
        let mut flow = Flow::new(0.0, GAP);
        flow.place(|x| Item::new(x + 40.0));
        let mut told = None;
        flow.place(|x| {
            told = Some(x);
            Item::new(5.0)
        });
        assert_eq!(told, Some(40.0 + GAP));
        // an absent item takes no room, no gap
        let mut flow = Flow::new(0.0, GAP);
        flow.push(Item::absent());
        assert_eq!((flow.next_x(), flow.end()), (0.0, 0.0));
    }
}
