//! **VStack** (and its one-at-a-time sibling [`VFlow`]) — the vertical counterpart of
//! [`crate::hstack`]: one run of items stacked top to bottom with ONE gap between neighbours, a
//! per-item override ([`Item::after`], [`Item::before`]) and the run anchored by its top, its bottom
//! or its middle.
//!
//! **It has no arithmetic of its own.** `VStack` wraps an [`HStack`] and `VFlow` a [`Flow`]; the axis
//! is only a name (an item's "width" is its HEIGHT here, a span's `x` is its `y`), so the gap rule,
//! the presence rule (an [`Item::absent`] takes no room and no gap), the summation order and every
//! float it returns are the engine's, and the two axes cannot drift. [`Item`] is axis-free and is
//! shared as is. Like the horizontal stack it measures nothing, so a replay gains no `Width`/`Height`
//! key: the caller says what each item's height MEANS (a cap band, a leading band, a measured block).
//!
//! The anchor is [`Anchor`], not `VAlign`: [`crate::label::VAlign`] is the text baseline choice and
//! screens import both.
//!
//! **Float order:** `Bottom`/`Centre` start at `anchor - height()`, where the heights are summed
//! before the gaps. A hand-written bottom-up sum interleaves them and can differ by an ulp at a
//! fractional cap height, so a site with an exact `assert_eq!` keeps its own order or checks it.
//!
//! Left off until a site needs them: `slot`/`budget`/`fit`/`kept`. A glyph's ink has no vertical
//! twin ([`Item::glyph`] is horizontal), so none is built.

use crate::hstack::{Align, Flow, HStack, Item, Span};
use crate::theme;

/// Where a vertical run is anchored, as a y.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    /// The first item's top edge stands at y; the run grows downward.
    Top(f32),
    /// The last item's bottom edge stands at y; the run grows upward.
    Bottom(f32),
    /// The run's middle stands at y.
    Centre(f32),
}

impl Anchor {
    const fn align(self) -> Align {
        match self {
            Anchor::Top(y) => Align::Leading(y),
            Anchor::Bottom(y) => Align::Trailing(y),
            Anchor::Centre(y) => Align::Centre(y),
        }
    }
}

/// Where one present item stands: the span it DRAWS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VSpan {
    /// Its index in the run (absent items leave gaps in the numbering).
    pub i: usize,
    /// Its top edge.
    pub y: f32,
    /// Its height: what the caller said it draws.
    pub h: f32,
    /// Where to draw the item's BOX (equal to `y`; there is no vertical glyph ink).
    pub box_y: f32,
}

impl From<Span> for VSpan {
    fn from(s: Span) -> Self {
        Self { i: s.i, y: s.x, h: s.w, box_y: s.box_x }
    }
}

/// A vertical run of [`Item`]s (each built from its HEIGHT), `gap` apart, anchored by [`Anchor`].
/// Holds no state and allocates nothing: build it where it is read.
#[derive(Clone, Copy, Debug)]
pub struct VStack<'a>(HStack<'a>);

impl<'a> VStack<'a> {
    /// A run of `items`, `gap` apart ([`Item::after`] / [`Item::before`] override it), anchored by `anchor`.
    pub const fn new(items: &'a [Item], gap: f32, anchor: Anchor) -> Self {
        Self(HStack::new(items, gap, anchor.align()))
    }

    /// A run of plain `heights`, each an [`Item::new`], `gap` apart.
    pub const fn heights(heights: &'a [f32], gap: f32, anchor: Anchor) -> Self {
        Self(HStack::widths(heights, gap, anchor.align()))
    }

    /// The run's height as DRAWN: every present item's height and one gap between present neighbours.
    pub const fn height(&self) -> f32 {
        self.0.width()
    }

    /// **THE placement**: every present item, in order, with the span it draws.
    pub fn placed(&self) -> impl Iterator<Item = VSpan> + 'a {
        self.0.placed().map(VSpan::from)
    }

    /// Item `i`'s drawn `(y, h)`, `None` when it is absent or past the end.
    pub fn span(&self, i: usize) -> Option<(f32, f32)> {
        self.0.span(i)
    }
}

/// **A vertical run placed as it is painted**: the top-anchored half of [`VStack::placed`], for a
/// run whose items are not in a slice. Same rules as the stack's.
#[derive(Clone, Copy, Debug)]
pub struct VFlow(Flow);

impl VFlow {
    /// A flow whose first item's top edge stands at `y`, `gap` between neighbours.
    pub const fn new(y: f32, gap: f32) -> Self {
        Self(Flow::new(y, gap))
    }

    /// Place the next item: the span it draws, `None` (and no room taken) when it is absent.
    pub fn push(&mut self, item: Item) -> Option<VSpan> {
        self.0.push(item).map(VSpan::from)
    }

    /// Place the next item, handing `make` the y its top will stand at BEFORE the item exists. The
    /// item must have no [`Item::before`] (debug-asserted); use [`Self::push`] for one that has.
    pub fn place(&mut self, make: impl FnOnce(f32) -> Item) -> Option<VSpan> {
        self.0.place(make).map(VSpan::from)
    }

    /// Where the next present item will stand, assuming it has no [`Item::before`].
    pub fn next_y(&self) -> f32 {
        self.0.next_x()
    }

    /// The bottom edge of the last present item (the top `y` while none is placed).
    pub const fn end(&self) -> f32 {
        self.0.end()
    }
}

// ---- The alert family's head ladder (lane vs-p0) ----

/// **The alert family's head ladder** ([`theme::alert`]): eyebrow, title, and an optional third line
/// (a file path, a dot-separated identity line), stacked from `top` with the alert gaps, as the
/// Tracks panel (ruled, [`AlertHead::ruled`]) and the person biography draw it. One builder, so
/// the measure function and the draw function read the SAME spans instead of each summing the bands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlertHead {
    /// The eyebrow: [`theme::alert::EYEBROW_LEAD`] tall.
    pub eyebrow: VSpan,
    /// The title, as tall as the caller said ([`theme::alert::TITLE_LEAD`] for a one-line panel title).
    pub title: VSpan,
    /// The third line, `None` when the caller passed no height for it.
    pub sub: Option<VSpan>,
    /// The rule under the ladder, `Some` only for [`AlertHead::ruled`].
    pub rule: Option<VSpan>,
    /// A zero-height marker where the content under the ruled ladder starts, `Some` only for
    /// [`AlertHead::ruled`].
    pub body: Option<VSpan>,
    /// The ladder's height: from `top` to the bottom of its last present line (for a ruled one, to
    /// [`Self::body`]).
    pub h: f32,
}

/// What stands under a ruled [`AlertHead`]: the air above the rule, the rule's own height and the
/// air between the rule and the content under it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadRule {
    pub gap_above: f32,
    pub h: f32,
    pub gap_below: f32,
}

impl AlertHead {
    /// The ladder from `top`: eyebrow, [`theme::alert::GAP_EYEBROW_TITLE`], a title `title_h` tall,
    /// [`theme::alert::GAP_TITLE_SUB`], then a third line `sub_h` tall when there is one (a panel that
    /// reserves the line whether or not it has text passes `Some` regardless).
    pub fn new(top: f32, title_h: f32, sub_h: Option<f32>) -> Self {
        Self::build(top, title_h, sub_h, None)
    }

    /// [`Self::new`] with the family's rule and the start of the content under it as two more items
    /// of the SAME stack: the rule `rule.gap_above` under the last line, a [`Self::body`] marker
    /// `rule.gap_below` under the rule. A panel that sits its body under the rule reads both from
    /// here instead of adding hand sums to the ladder.
    pub fn ruled(top: f32, title_h: f32, sub_h: Option<f32>, rule: HeadRule) -> Self {
        Self::build(top, title_h, sub_h, Some(rule))
    }

    fn build(top: f32, title_h: f32, sub_h: Option<f32>, rule: Option<HeadRule>) -> Self {
        let above = rule.map_or(0.0, |r| r.gap_above);
        let items = [
            Item::new(theme::alert::EYEBROW_LEAD),
            Item::new(title_h).after(theme::alert::GAP_TITLE_SUB),
            sub_h.map_or(Item::absent(), |h| Item::new(h).after(above)),
            rule.map_or(Item::absent(), |r| Item::new(r.h).after(r.gap_below)),
            rule.map_or(Item::absent(), |_| Item::new(0.0)),
        ];
        let stack = VStack::new(&items, theme::alert::GAP_EYEBROW_TITLE, Anchor::Top(top));
        let mut spans = stack.placed();
        let (Some(eyebrow), Some(title)) = (spans.next(), spans.next()) else {
            unreachable!("the eyebrow and the title are always present")
        };
        let sub = stack.span(2).map(|(y, h)| VSpan { i: 2, y, h, box_y: y });
        let rule = stack.span(3).map(|(y, h)| VSpan { i: 3, y, h, box_y: y });
        let body = stack.span(4).map(|(y, h)| VSpan { i: 4, y, h, box_y: y });
        Self { eyebrow, title, sub, rule, body, h: stack.height() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAP: f32 = 10.0;

    fn ys(stack: &VStack<'_>) -> Vec<(usize, f32, f32)> {
        stack.placed().map(|p| (p.i, p.y, p.h)).collect()
    }

    fn run(heights: &[f32]) -> Vec<Item> {
        heights.iter().map(|&h| Item::new(h)).collect()
    }

    /// The gap between `a` then `b` is `b.before`, else `a.after`, else the stack's.
    #[test]
    fn before_beats_after_beats_the_stack_gap() {
        let items = [
            Item::new(10.0).after(7.0),
            Item::new(10.0).after(7.0).before(3.0),
            Item::new(10.0).after(5.0),
            Item::new(10.0),
            Item::new(10.0).before(2.0),
        ];
        let st = VStack::new(&items, GAP, Anchor::Top(0.0));
        // 0 -> 1: before 3 wins over after 7; 1 -> 2: no before, after 7; 2 -> 3: after 5; 3 -> 4: before 2
        assert_eq!(ys(&st), vec![(0, 0.0, 10.0), (1, 13.0, 10.0), (2, 30.0, 10.0), (3, 45.0, 10.0), (4, 57.0, 10.0)]);
        assert_eq!(st.height(), 67.0);
    }

    /// An absent item neither gives nor takes a gap: the `before` of the item after it is measured
    /// from the item before it, and the absent item's own `before`/`after` are never read.
    #[test]
    fn an_absent_item_with_before_gives_and_takes_nothing() {
        let items = [Item::new(10.0), Item::absent().before(99.0).after(99.0), Item::new(10.0).before(4.0)];
        let st = VStack::new(&items, GAP, Anchor::Top(0.0));
        assert_eq!(ys(&st), vec![(0, 0.0, 10.0), (2, 14.0, 10.0)]);
        // …and without a `before` on the survivor the stack gap applies, not the absent item's.
        let items = [Item::new(10.0), Item::absent().before(99.0), Item::new(10.0)];
        assert_eq!(ys(&VStack::new(&items, GAP, Anchor::Top(0.0))), vec![(0, 0.0, 10.0), (2, 20.0, 10.0)]);
    }

    /// `before` on the first present item is ignored: nothing precedes it.
    #[test]
    fn before_on_the_first_item_is_ignored() {
        let items = [Item::absent(), Item::new(10.0).before(50.0)];
        assert_eq!(ys(&VStack::new(&items, GAP, Anchor::Top(5.0))), vec![(1, 5.0, 10.0)]);
    }

    #[test]
    fn top_bottom_and_centre_anchor_the_same_run() {
        let items = run(&[10.0, 20.0, 30.0]);
        let h = 60.0 + 2.0 * GAP;
        let top = VStack::new(&items, GAP, Anchor::Top(100.0));
        let bot = VStack::new(&items, GAP, Anchor::Bottom(200.0));
        let mid = VStack::new(&items, GAP, Anchor::Centre(150.0));
        assert_eq!(top.height(), h);
        assert_eq!(ys(&top), vec![(0, 100.0, 10.0), (1, 120.0, 20.0), (2, 150.0, 30.0)]);
        assert_eq!(ys(&bot), vec![(0, 200.0 - h, 10.0), (1, 200.0 - h + 20.0, 20.0), (2, 200.0 - h + 50.0, 30.0)]);
        assert_eq!(ys(&mid), vec![(0, 150.0 - h * 0.5, 10.0), (1, 150.0 - h * 0.5 + 20.0, 20.0), (2, 150.0 - h * 0.5 + 50.0, 30.0)]);
        // the bottom of the last item stands on the Bottom anchor
        let last = bot.placed().last().unwrap();
        assert_eq!(last.y + last.h, 200.0);
        assert_eq!(bot.span(1), Some((200.0 - h + 20.0, 20.0)));
        assert_eq!(bot.span(7), None);
    }

    #[test]
    fn heights_reads_plain_heights() {
        let hs = [10.0, 20.0];
        let st = VStack::heights(&hs, GAP, Anchor::Top(0.0));
        assert_eq!(ys(&st), vec![(0, 0.0, 10.0), (1, 20.0, 20.0)]);
    }

    /// A `VFlow` fed the same items one at a time stands them where `VStack` does, `before` included.
    #[test]
    fn vflow_agrees_with_vstack() {
        let items = [
            Item::new(12.0).after(6.0),
            Item::absent().before(40.0),
            Item::new(8.0).before(3.0),
            Item::new(30.0),
            Item::new(9.0).after(1.0),
            Item::new(4.0),
        ];
        let st = VStack::new(&items, GAP, Anchor::Top(17.0));
        let mut flow = VFlow::new(17.0, GAP);
        let flowed: Vec<VSpan> = items.iter().filter_map(|&it| flow.push(it)).collect();
        assert_eq!(flowed, st.placed().collect::<Vec<_>>());
        let last = flowed.last().unwrap();
        assert_eq!(flow.end(), last.y + last.h);
        assert_eq!(st.height(), flow.end() - 17.0);
    }

    #[test]
    fn vflow_place_hands_make_the_pen_and_next_y_agrees() {
        let mut f = VFlow::new(0.0, GAP);
        assert_eq!(f.next_y(), 0.0);
        f.push(Item::new(10.0));
        assert_eq!(f.next_y(), 20.0);
        let mut seen = None;
        let s = f.place(|y| {
            seen = Some(y);
            Item::new(5.0)
        });
        assert_eq!(seen, Some(20.0));
        assert_eq!(s.unwrap().y, 20.0);
        assert_eq!(f.end(), 25.0);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "cannot honour Item::before")]
    fn vflow_place_refuses_a_before() {
        VFlow::new(0.0, GAP).place(|_| Item::new(5.0).before(3.0));
    }

    /// **About panel's ladder as a fixture** (`screens::about_panel::stack`): eyebrow, synopsis,
    /// tagline, rule, footer, with 24 / 20 / 32 / 24 between, where a missing synopsis leaves the
    /// eyebrow-to-tagline gap at 20 (the tagline's own), never the stack's 24, and a missing tagline
    /// leaves the rule 32 under whatever precedes it.
    #[test]
    fn about_panels_ladder_keeps_its_gaps_when_blocks_are_absent() {
        const PAD: f32 = 48.0;
        const RULE_H: f32 = 1.0;
        const FOOT_H: f32 = 30.0;
        const EYEBROW: f32 = theme::alert::EYEBROW_LEAD;
        // (synopsis, tagline) -> the hand-summed `about_panel::stack` y's it replaced
        let oracle = |syn: f32, tag: f32| {
            let mut y = PAD;
            let eyebrow = y;
            y += EYEBROW;
            let mut synopsis = 0.0;
            if syn > 0.0 {
                synopsis = y + 24.0;
                y = synopsis + syn;
            }
            let mut tagline = 0.0;
            if tag > 0.0 {
                tagline = y + 20.0;
                y = tagline + tag;
            }
            let rule = y + 32.0;
            y = rule + RULE_H;
            let footer = y + 24.0;
            (eyebrow, synopsis, tagline, rule, footer, footer + FOOT_H)
        };
        for (syn, tag) in [(0.0, 0.0), (126.0, 0.0), (0.0, 30.0), (126.0, 30.0)] {
            let items = [
                Item::new(EYEBROW),
                if syn > 0.0 { Item::new(syn) } else { Item::absent() },
                if tag > 0.0 { Item::new(tag).before(20.0) } else { Item::absent() },
                Item::new(RULE_H).before(32.0),
                Item::new(FOOT_H).before(24.0),
            ];
            // the synopsis keeps the stack's own 24
            let st = VStack::new(&items, 24.0, Anchor::Top(PAD));
            let at = |i| st.span(i).map_or(0.0, |(y, _)| y);
            let (e, s, t, r, f, end) = oracle(syn, tag);
            assert_eq!((at(0), at(1), at(2), at(3), at(4)), (e, s, t, r, f), "syn {syn} tag {tag}");
            let (fy, fh) = st.span(4).unwrap();
            assert_eq!(fy + fh, end);
        }
    }

    /// The head ladder lands where the bands sum to, with and without the third line.
    #[test]
    fn alert_head_stacks_the_alert_bands() {
        use theme::alert::*;
        let two = AlertHead::new(100.0, TITLE_LEAD, None);
        assert_eq!(two.eyebrow, VSpan { i: 0, y: 100.0, h: EYEBROW_LEAD, box_y: 100.0 });
        assert_eq!(two.title.y, 100.0 + EYEBROW_LEAD + GAP_EYEBROW_TITLE);
        assert_eq!(two.title.h, TITLE_LEAD);
        assert_eq!(two.sub, None);
        assert_eq!(two.h, EYEBROW_LEAD + GAP_EYEBROW_TITLE + TITLE_LEAD);

        let path_h = 20.8;
        let three = AlertHead::new(100.0, TITLE_LEAD, Some(path_h));
        let sub = three.sub.expect("third line");
        assert_eq!(sub.i, 2);
        assert_eq!(sub.y, three.title.y + TITLE_LEAD + GAP_TITLE_SUB);
        assert_eq!(sub.h, path_h);
        assert_eq!(three.h, EYEBROW_LEAD + TITLE_LEAD + path_h + GAP_EYEBROW_TITLE + GAP_TITLE_SUB);
        assert_eq!(sub.y + sub.h, 100.0 + three.h);
        // the first two lines do not move when the third arrives
        assert_eq!((three.eyebrow, three.title), (two.eyebrow, two.title));
    }
}
