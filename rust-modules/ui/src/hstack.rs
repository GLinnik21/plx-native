//! **HStack** — one horizontal run of items a caller has already measured, spaced the way
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
//! - a glyph or plate's box;
//! - a spring's value, mid-flight (`Item::new(spring.pos)`): the stack has no state, so the items
//!   after a growing one stand where the value puts them on THIS frame, the way `Column::height`
//!   takes a spring. The motion belongs to whoever owns the spring (`TabStrip`'s `SelMark` is the
//!   row of pills' only glide); the stack just reads the present value.
//!
//! The gap is a token the caller passes (`theme::space::*`, `page_header::GAP`), never a literal here.
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
//! `PageHeader::new` measures, and `Item::absent` has no caller yet.) `width` and `placed` skip it,
//! budget and gap both.

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
}

impl Item {
    /// A present item `w` wide. A `w` of 0 is still present, and keeps its gap.
    pub const fn new(w: f32) -> Self {
        Self { w, present: true, budget: None, after: None }
    }

    /// An item that is not on the row: no width, no gap. Give it [`Self::slot`] and it still
    /// reserves its room in [`HStack::budget_w`].
    pub const fn absent() -> Self {
        Self { w: 0.0, present: false, budget: None, after: None }
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
    /// Its index in the slice the stack was built from (absent items leave gaps in the numbering).
    pub i: usize,
    /// Its leading edge.
    pub x: f32,
    /// Its width: what the caller said it draws.
    pub w: f32,
}

/// A run of [`Item`]s with one gap between neighbours, anchored by [`Align`]. It borrows the slice,
/// holds no state and allocates nothing: build it where it is read, as often as that is.
#[derive(Clone, Copy, Debug)]
pub struct HStack<'a> {
    items: &'a [Item],
    gap: f32,
    align: Align,
}

impl<'a> HStack<'a> {
    /// A run of `items`, `gap` apart (an item's [`Item::after`] overrides it for that one), anchored
    /// by `align`.
    pub const fn new(items: &'a [Item], gap: f32, align: Align) -> Self {
        Self { items, gap, align }
    }

    /// The gap after `item`.
    fn gap_after(&self, item: &Item) -> f32 {
        item.after.unwrap_or(self.gap)
    }

    /// The sum of `size` over the items that count (`Some`), plus one gap between each neighbouring
    /// pair of them. The widths are added first and the gaps second, so a caller that summed a row
    /// as "runs, then gaps" gets the same float back.
    fn sum(&self, size: impl Fn(&Item) -> Option<f32>) -> f32 {
        let (mut runs, mut gaps) = (0.0, 0.0);
        let mut before: Option<&Item> = None;
        for item in self.items {
            let Some(w) = size(item) else { continue };
            runs += w;
            if let Some(prev) = before {
                gaps += self.gap_after(prev);
            }
            before = Some(item);
        }
        runs + gaps
    }

    /// The run's width as DRAWN: every present item's width and one gap between present
    /// neighbours. An absent item adds neither; a present item of width 0 adds its gaps.
    pub fn width(&self) -> f32 {
        self.sum(|item| item.present.then_some(item.w))
    }

    /// What the run RESERVES: [`Self::width`] with every item at the larger of what it draws and
    /// its [`Item::slot`], and an absent item that has a slot counted at its slot, gap included.
    /// This sizes the room a neighbour may use; it is never where anything stands.
    pub fn budget_w(&self) -> f32 {
        self.sum(|item| match (item.present, item.budget) {
            (true, budget) => Some(budget.map_or(item.w, |b| item.w.max(b))),
            (false, budget) => budget,
        })
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
        let items: &'a [Item] = self.items;
        let gap = self.gap;
        let mut x = self.start();
        let mut advance: Option<f32> = None;
        items.iter().enumerate().filter_map(move |(i, item)| {
            if !item.present {
                return None;
            }
            if let Some(by) = advance {
                x += by;
            }
            advance = Some(item.w + item.after.unwrap_or(gap));
            Some(Span { i, x, w: item.w })
        })
    }

    /// Item `i`'s drawn `(x, w)`, `None` when it is absent or past the end: what a `TabStrip`'s span
    /// closure and a hit test ask for.
    pub fn span(&self, i: usize) -> Option<(f32, f32)> {
        self.placed().find(|placed| placed.i == i).map(|placed| (placed.x, placed.w))
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
}
