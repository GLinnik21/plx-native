//! `Focusable` for the widgets (restructure spec §7.1, phase 3a): geometry IS `Focusable::place`.
//!
//! Each widget answers the query protocol through a small VIEW over its live state plus the
//! frame arguments its draw takes — [`Shelf`] over a `CardRow`, [`Table`] over a `TableView`,
//! [`TabRow`] over the shared top strip's drawn pills, [`Grid`] over a column of shelves, and
//! [`Document`] over a `DocumentReader` — built by the screen for the frame, exactly as it builds
//! the draw call. The rule that makes these honest is that the DRAW reads the same formula:
//! `card_row::tile_rect` is what `strip` places tiles by and what `Shelf::place` answers;
//! `TableView::row_frame` is what `row_rect`/`hit_row` walk. A host test per widget pins the two
//! against each other, so a change to a draw's geometry that forgets `place` fails here.
//!
//! Elements are INDICES here (`IndexElem`): the engine (3b) maps a screen's `ElemKey` onto them.
//! `seat` takes no source index yet (the `from` tie-break of `column_near_x` is the engine's to
//! supply); it projects the source centre onto the lattice with the projected index as its own
//! tie-break, which is stable for every non-tie and the engine's job for the tie.

#![allow(dead_code)] // phase 3a: the screens compose through these from 3b (the engine) and 5b on

use super::machine::{Cx, EntryId, FocusKey, GroupId, Host};
use super::screen::{At, AxisMask, Dir, EdgeRule, Focusable, GroupKind, GroupSpec, Placed, Seat, Step};
use super::{card_row, Rect};

/// An element key that is an INDEX into a widget's items. `u32` is one (the fixture host); the
/// application's `ElemKey` implements it for its `Slot`/`Item` variants in 3b.
pub trait IndexElem: Copy {
    fn index(self) -> Option<u32>;
    fn of_index(i: u32) -> Self;
}

impl IndexElem for u32 {
    fn index(self) -> Option<u32> {
        Some(self)
    }
    fn of_index(i: u32) -> Self {
        i
    }
}

fn key<K: IndexElem>(entry: EntryId, i: usize) -> FocusKey<K> {
    FocusKey {
        entry,
        elem: K::of_index(i as u32),
    }
}

fn step_index<K: IndexElem>(entry: EntryId, k: FocusKey<K>, dir: Dir, n: usize, axis_h: bool) -> Step<K> {
    let Some(i) = k.elem.index() else {
        return Step::Edge;
    };
    let i = i as usize;
    let (back, fwd) = if axis_h {
        (Dir::Left, Dir::Right)
    } else {
        (Dir::Up, Dir::Down)
    };
    if dir == back && i > 0 {
        Step::Move(key(entry, i - 1))
    } else if dir == fwd && i + 1 < n {
        Step::Move(key(entry, i + 1))
    } else {
        Step::Edge
    }
}

fn clamp_index<K: IndexElem>(entry: EntryId, want: FocusKey<K>, n: usize) -> FocusKey<K> {
    let i = want.elem.index().unwrap_or(0) as usize;
    key(entry, i.min(n.saturating_sub(1)))
}

/// One shelf (`CardRow`) as the draw sees it this frame: the same arguments `card_row::strip`
/// takes, so `place` and the drawn tile are one formula.
pub struct Shelf<'a> {
    pub row: &'a card_row::CardRow,
    pub n: usize,
    pub sty: &'a card_row::RowStyle,
    pub row_y: f32,
    pub size: (f32, f32),
    pub pitch: f32,
    pub group: GroupId,
    pub entry: EntryId,
    /// The row's horizontal extent (the panel width for a full-bleed shelf).
    pub extent: Rect,
}

impl<H: Host> Focusable<H> for Shelf<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Nearest,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: self.extent,
            len: self.n,
        });
    }
    fn neighbour(&self, k: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        step_index(self.entry, k, dir, self.n, true)
    }
    fn place(&self, key: &H::Elem, _cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let i = key.index()? as usize;
        if i >= self.n {
            return None;
        }
        let rest = card_row::tile_rect(i, self.sty.margin_x, self.pitch, self.row.scroll_x(), self.row_y, self.size);
        let s = match at {
            At::Drawn => self.row.scale(i),
            At::SpringTarget => {
                if self.row.focus() == i as i32 {
                    self.sty.focus_scale
                } else {
                    1.0
                }
            }
        };
        Some(Placed {
            rect: rest.scaled(s),
            rest_rect: rest,
            clip: self.extent,
        })
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        clamp_index(self.entry, want, self.n)
    }
    fn seat(&self, _g: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let cx_ = from.rect.x + from.rect.w * 0.5;
        let guess = ((cx_ - self.sty.margin_x + self.row.scroll_x()) / self.pitch).max(0.0) as usize;
        let i = card_row::column_near_x(cx_, self.sty.margin_x, self.pitch, self.size.0, self.row.scroll_x(), self.n, guess);
        key(self.entry, i)
    }
}

/// A `TableView` in its frame: a `Column` group whose rows skip the separators, as `move_sel`
/// does; `place` is `TableView::row_frame`, the walk `row_rect` and `hit_row` share.
pub struct Table<'a> {
    pub table: &'a super::table::TableView,
    pub frame: Rect,
    pub group: GroupId,
    pub entry: EntryId,
}

impl<H: Host> Focusable<H> for Table<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: self.frame,
            len: self.table.n_rows().max(0) as usize,
        });
    }
    fn neighbour(&self, k: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        let Some(i) = k.elem.index() else {
            return Step::Edge;
        };
        let delta = match dir {
            Dir::Up => -1,
            Dir::Down => 1,
            _ => return Step::Edge,
        };
        match self.table.next_selectable(i as i32, delta) {
            Some(j) => Step::Move(key(self.entry, j as usize)),
            None => Step::Edge,
        }
    }
    fn place(&self, key: &H::Elem, _cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        let r = self.table.row_frame(self.frame, key.index()? as i32)?;
        Some(Placed {
            rect: r,
            rest_rect: r,
            clip: self.frame,
        })
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let i = want.elem.index().unwrap_or(0) as i32;
        key(self.entry, self.table.settle(i).max(0) as usize)
    }
    fn seat(&self, _g: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let cy = from.rect.y + from.rect.h * 0.5;
        let mut best = (f32::MAX, 0i32);
        for i in 0..self.table.n_rows() {
            if let Some(r) = self.table.row_frame(self.frame, i) {
                if self.table.next_selectable(i, 0) != Some(i) {
                    continue;
                }
                let d = (r.y + r.h * 0.5 - cy).abs();
                if d < best.0 {
                    best = (d, i);
                }
            }
        }
        key(self.entry, best.1.max(0) as usize)
    }
}

/// The shared top strip's pills as DRAWN (their recorded rects): a `Row` group the container
/// contributes above the page's (spec §6.2). Until the pills come from `Cx`, the rects are read
/// off the last draw (`widgets::tab_pill_rects`).
pub struct TabRow<'a> {
    pub rects: &'a [Rect],
    pub group: GroupId,
    pub entry: EntryId,
}

impl<H: Host> Focusable<H> for TabRow<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let extent = self
            .rects
            .iter()
            .fold(None::<Rect>, |acc, r| Some(acc.map_or(*r, |a| a.union(*r))))
            .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Nearest,
            reachable: AxisMask::VERTICAL,
            edge: [EdgeRule::Geometric; 4],
            extent,
            len: self.rects.len(),
        });
    }
    fn neighbour(&self, k: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        step_index(self.entry, k, dir, self.rects.len(), true)
    }
    fn place(&self, key: &H::Elem, _cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        let r = *self.rects.get(key.index()? as usize)?;
        Some(Placed {
            rect: r,
            rest_rect: r,
            clip: Rect::FULL,
        })
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        clamp_index(self.entry, want, self.rects.len())
    }
    fn seat(&self, _g: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let cx_ = from.rect.x + from.rect.w * 0.5;
        let mut best = (f32::MAX, 0usize);
        for (i, r) in self.rects.iter().enumerate() {
            let d = (r.x + r.w * 0.5 - cx_).abs();
            if d < best.0 {
                best = (d, i);
            }
        }
        key(self.entry, best.1)
    }
}

/// A column of shelves (the home grid, the Library's shelves): one `Row` group per shelf, so a
/// vertical move is the §7.3 step-3 geometric search landing by `Seat::Nearest` — which is
/// `card_row::column_near_x`'s contract, the rule the home grid has always used. Element keys
/// are `row * stride + col`.
pub struct Grid<'a> {
    pub shelves: &'a [Shelf<'a>],
    /// Keys are `row * stride + col`; `stride` bounds a row's length.
    pub stride: u32,
    pub entry: EntryId,
}

impl<'a> Grid<'a> {
    fn split(&self, k: u32) -> (usize, usize) {
        ((k / self.stride) as usize, (k % self.stride) as usize)
    }
    fn join(&self, r: usize, c: usize) -> u32 {
        r as u32 * self.stride + c as u32
    }
}

impl<H: Host> Focusable<H> for Grid<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        for s in self.shelves {
            Focusable::<H>::groups(s, cx, out);
        }
    }
    fn neighbour(&self, k: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        let Some(i) = k.elem.index() else {
            return Step::Edge;
        };
        let (r, c) = self.split(i);
        let Some(s) = self.shelves.get(r) else {
            return Step::Edge;
        };
        match dir {
            Dir::Left if c > 0 => Step::Move(key(self.entry, self.join(r, c - 1) as usize)),
            Dir::Right if c + 1 < s.n => Step::Move(key(self.entry, self.join(r, c + 1) as usize)),
            _ => Step::Edge,
        }
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let (r, c) = self.split(key.index()?);
        let s = self.shelves.get(r)?;
        Focusable::<H>::place(s, &H::Elem::of_index(c as u32), cx, at)
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let (r, c) = self.split(want.elem.index().unwrap_or(0));
        let r = r.min(self.shelves.len().saturating_sub(1));
        let n = self.shelves.get(r).map_or(0, |s| s.n);
        key(self.entry, self.join(r, c.min(n.saturating_sub(1))) as usize)
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let Some((r, s)) = self.shelves.iter().enumerate().find(|(_, s)| s.group == g) else {
            return key(self.entry, 0);
        };
        let seated: FocusKey<H::Elem> = Focusable::<H>::seat(s, g, from, cx);
        let c = seated.elem.index().unwrap_or(0) as usize;
        key(self.entry, self.join(r, c) as usize)
    }
}

/// A `DocumentReader` in its frame: a `Document` group of ONE element that scrolls inside and
/// leaves only at its ends (spec §7.3 step 2).
pub struct Document<'a> {
    pub reader: &'a super::document_reader::DocumentReader,
    pub frame: Rect,
    pub group: GroupId,
    pub entry: EntryId,
}

impl<H: Host> Focusable<H> for Document<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Document,
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: self.frame,
            len: 1,
        });
    }
    fn neighbour(&self, k: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        // A move INSIDE the document is a scroll the owner performs on `FocusMoved`; the key
        // stays the document's one element. At an end, the direction leaves.
        match dir {
            Dir::Up if !self.reader.at_top() => Step::Move(k),
            Dir::Down if !self.reader.at_end() => Step::Move(k),
            _ => Step::Edge,
        }
    }
    fn place(&self, key: &H::Elem, _cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        (key.index()? == 0).then_some(Placed {
            rect: self.frame,
            rest_rect: self.frame,
            clip: self.frame,
        })
    }
    fn reconcile(&self, _want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        key(self.entry, 0)
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        key(self.entry, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixture::{FixtureHost, FixtureMeasure, FixtureView, FixtureViews};
    use crate::ui::machine::{FocusRead, InputOwner, PressRead, Tick};

    fn cx<'a>(m: &'a FixtureMeasure, v: &'a FixtureView) -> Cx<'a, FixtureHost> {
        Cx {
            views: FixtureViews { store: v },
            tick: Tick::default(),
            measure: m,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: InputOwner::Entry(EntryId(0)),
        }
    }

    const E: EntryId = EntryId(3);

    /// The shelf's `place` is the strip's tile formula: the same rect `card_row::strip` draws
    /// tile `i` at, popped by the same spring.
    #[test]
    fn a_shelf_places_a_tile_where_the_strip_draws_it() {
        let m = FixtureMeasure;
        let v = FixtureView::default();
        let cx = cx(&m, &v);
        let row = card_row::CardRow::new();
        let sty = card_row::RowStyle::HOME;
        let pitch = sty.w + sty.gap;
        let shelf = Shelf {
            row: &row,
            n: 8,
            sty: &sty,
            row_y: 300.0,
            size: (sty.w, sty.h),
            pitch,
            group: GroupId(1),
            entry: E,
            extent: Rect::FULL,
        };
        for i in [0usize, 3, 7] {
            let p = Focusable::<FixtureHost>::place(&shelf, &(i as u32), &cx, At::Drawn).unwrap();
            let want = card_row::tile_rect(i, sty.margin_x, pitch, row.scroll_x(), 300.0, (sty.w, sty.h));
            assert_eq!((p.rest_rect.x, p.rest_rect.y, p.rest_rect.w, p.rest_rect.h), (want.x, want.y, want.w, want.h));
        }
        assert!(Focusable::<FixtureHost>::place(&shelf, &8u32, &cx, At::Drawn).is_none());
        let k = FocusKey { entry: E, elem: 7u32 };
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&shelf, k, Dir::Right, &cx), Step::Edge));
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&shelf, k, Dir::Left, &cx), Step::Move(_)));
        // seating from a rect over tile 3's centre lands on 3
        let from = Focusable::<FixtureHost>::place(&shelf, &3u32, &cx, At::Drawn).unwrap();
        assert_eq!(Focusable::<FixtureHost>::seat(&shelf, GroupId(1), from, &cx).elem, 3);
    }

    /// A grid of two shelves of different lengths: DOWN off the longer row seats by the
    /// column-near-x contract, and a key past a row's end reconciles onto its last tile.
    #[test]
    fn a_grid_seats_by_the_column_near_x_contract() {
        let m = FixtureMeasure;
        let v = FixtureView::default();
        let cx = cx(&m, &v);
        let rows = [card_row::CardRow::new(), card_row::CardRow::new()];
        let sty = card_row::RowStyle::HOME;
        fn mk<'a>(r: &'a card_row::CardRow, sty: &'a card_row::RowStyle, n: usize, y: f32, g: u32) -> Shelf<'a> {
            let pitch = sty.w + sty.gap;
            Shelf {
            row: r,
            n,
            sty,
            row_y: y,
            size: (sty.w, sty.h),
            pitch,
            group: GroupId(g),
            entry: E,
            extent: Rect::FULL,
            }
        }
        let shelves = [mk(&rows[0], &sty, 10, 100.0, 1), mk(&rows[1], &sty, 4, 600.0, 2)];
        let grid = Grid {
            shelves: &shelves,
            stride: 64,
            entry: E,
        };
        let from = Focusable::<FixtureHost>::place(&grid, &(0 * 64 + 6), &cx, At::Drawn).unwrap();
        let seated = Focusable::<FixtureHost>::seat(&grid, GroupId(2), from, &cx);
        assert_eq!(seated.elem, 64 + 3, "clamped to the shorter row's last tile");
        let want = FocusKey { entry: E, elem: 64 + 9 };
        assert_eq!(Focusable::<FixtureHost>::reconcile(&grid, want, &cx).elem, 64 + 3);
    }

    /// A table's `place` is the walk `row_rect` shares; separators are stepped over.
    #[test]
    fn a_table_places_rows_by_the_one_walk_and_skips_separators() {
        use crate::ui::table::{Row, Section, TableView};
        let m = FixtureMeasure;
        let v = FixtureView::default();
        let cx = cx(&m, &v);
        let mut t = TableView::new();
        t.set_sections(
            vec![Section::new("").row(Row::new("a")).row(Row::separator()).row(Row::new("b")).row(Row::new("c"))],
            0,
            false,
        );
        let frame = Rect::new(100.0, 100.0, 600.0, 800.0);
        let table = Table {
            table: &t,
            frame,
            group: GroupId(5),
            entry: E,
        };
        let p0 = Focusable::<FixtureHost>::place(&table, &0u32, &cx, At::Drawn).unwrap();
        assert_eq!(p0.rect.x, t.row_rect(frame, 0).unwrap().x);
        let k0 = FocusKey { entry: E, elem: 0u32 };
        match Focusable::<FixtureHost>::neighbour(&table, k0, Dir::Down, &cx) {
            Step::Move(k) => assert_eq!(k.elem, 2, "the separator at 1 is stepped over"),
            Step::Edge => panic!("down from the first row is a move"),
        }
        let k3 = FocusKey { entry: E, elem: 3u32 };
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&table, k3, Dir::Down, &cx), Step::Edge));
        let sep = FocusKey { entry: E, elem: 1u32 };
        assert_ne!(Focusable::<FixtureHost>::reconcile(&table, sep, &cx).elem, 1, "a separator is never focus");
    }

    /// A document scrolls inside and leaves only at its ends.
    #[test]
    fn a_document_scrolls_inside_and_leaves_at_its_ends() {
        let m = FixtureMeasure;
        let v = FixtureView::default();
        let cx = cx(&m, &v);
        let mut reader = super::super::document_reader::DocumentReader::new();
        let frame = Rect::new(0.0, 0.0, 800.0, 600.0);
        fn doc(r: &super::super::document_reader::DocumentReader, frame: Rect) -> Document<'_> {
            Document {
                reader: r,
                frame,
                group: GroupId(9),
                entry: E,
            }
        }
        let k = FocusKey { entry: E, elem: 0u32 };
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&doc(&reader, frame), k, Dir::Up, &cx), Step::Edge), "at the top, UP leaves");
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&doc(&reader, frame), k, Dir::Down, &cx), Step::Edge), "an empty document has no inside");
        reader.set_extent_for_test(2000.0);
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&doc(&reader, frame), k, Dir::Down, &cx), Step::Move(_)));
        reader.move_by(1000);
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&doc(&reader, frame), k, Dir::Down, &cx), Step::Edge), "at the end, DOWN leaves");
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&doc(&reader, frame), k, Dir::Up, &cx), Step::Move(_)));
    }
}
