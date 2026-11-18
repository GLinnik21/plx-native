//! `Focusable` for the widgets (restructure spec §7.1, phase 3a): geometry IS `Focusable::place`.
//!
//! Each widget answers the query protocol through a small VIEW over its live state plus the
//! frame arguments its draw takes — [`Shelf`] over a `CardRow`, [`Table`] over a `TableView`,
//! [`TabRow`] over the shared top strip's drawn pills, [`Grid`] over a column of shelves, and
//! [`Document`] over a `DocumentReader` — built by the screen for the frame, exactly as it builds
//! the draw call. The rule that makes these honest is that the DRAW reads the same formula:
//! `card_row::tile_rect` is what `cards::Shelf` places tiles by and what `Shelf::place` answers;
//! `TableView::row_frame` is what `row_rect`/`hit_row` walk. A host test per widget pins the two
//! against each other, so a change to a draw's geometry that forgets `place` fails here.
//!
//! Elements are INDICES here (`IndexElem`): the engine (3b) maps a screen's `ElemKey` onto them.
//! `seat` takes no source index yet (the `from` tie-break of `column_near_x` is the engine's to
//! supply); it projects the source centre onto the lattice with the projected index as its own
//! tie-break, which is stable for every non-tie and the engine's job for the tie.

#![allow(dead_code)] // phase 3a: the screens compose through these from 3b (the engine) and 5b on

use plx_machine::machine::{Cx, EntryId, FocusKey, GroupId, Host};
use super::a11y::{A11y, A11yPath, Role, Seg};
use super::screen::{At, AxisMask, Dir, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, Seat, Step};
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
    if dir != back && dir != fwd { return Step::Edge; }
    super::focus::next_enabled_index(i, n, dir == fwd, |_| true)
        .map_or(Step::Edge, |i| Step::Move(key(entry, i)))
}

/// **Where a plain index ends and a key namespace begins.** A widget's items are keyed by their own
/// index (`0..n`); everything else that shares the `u32` key space — the container's strip
/// (`dispatch::STRIP_BASE`), a screen's footer / keypad / read-out controls, a settings band —
/// carves its range at or above this, "far above any realistic roster length". The one rule it
/// buys: a key at or above it is NOT an index into any widget and must never be treated as one.
pub const NAMESPACE_FLOOR: u32 = 0x1000_0000;

/// **The one reconcile rule for an index-keyed widget** that is asked about a key it may not own.
///
/// - an index inside `0..n` is kept;
/// - a stale index past the end (the list shrank under the cursor) clamps to the LAST item, the
///   nearest survivor;
/// - a key from another namespace (see [`NAMESPACE_FLOOR`]) was never an index, so "clamp it to
///   the end" is meaningless and lands focus on the last item by accident — it re-seats on the
///   FIRST, the widget's own default.
///
/// Every `reconcile` that used to write `i.min(n - 1)` on a key it had not checked is this
/// function. The failure it retires: the tab strip's pill (`STRIP_BASE + 4`) was focused for a
/// frame, the strip left, and the who's-watching picker clamped the pill to its last avatar.
pub fn clamp_plain_index(i: u32, n: usize) -> usize {
    if i >= NAMESPACE_FLOOR {
        0
    } else {
        (i as usize).min(n.saturating_sub(1))
    }
}

fn clamp_index<K: IndexElem>(entry: EntryId, want: FocusKey<K>, n: usize) -> FocusKey<K> {
    let i = want.elem.index().unwrap_or(0);
    key(entry, clamp_plain_index(i, n))
}

/// The index `i` of `name.<i>` when `path` is exactly that two-segment RELATIVE path and `i < n`:
/// what every indexed widget's `a11y_find` reads (`card.3`, `tab.0`, `band.1`), so none of them
/// scans. (A table's `row.<n>` reads it for the parse alone and resolves through [`table_elems`],
/// `table_a11y_find`.) A qualified path never matches (the owner view strips the namespace before it hands a
/// path down), the same rule [`crate::a11y::default_walk`] keeps by comparing whole paths.
pub(crate) fn indexed(path: &A11yPath, name: &str, n: usize) -> Option<usize> {
    if path.ns().is_some() {
        return None;
    }
    let mut segs = path.segs();
    match (segs.next(), segs.next(), segs.next()) {
        (Some(Seg::Name(s)), Some(Seg::Index(i)), None) if s == name && i < n => Some(i),
        _ => None,
    }
}

/// The name of a one-segment RELATIVE path (`document`, `sort`), else `None`.
pub(crate) fn sole_name(path: &A11yPath) -> Option<&str> {
    if path.ns().is_some() {
        return None;
    }
    let mut segs = path.segs();
    match (segs.next(), segs.next()) {
        (Some(Seg::Name(name)), None) => Some(name),
        _ => None,
    }
}

/// Row `index` of `t` in layout order (section headers are not rows), as `TableView::n_rows` counts.
pub fn table_row(t: &super::table::TableView, index: usize) -> Option<&super::table::Row> {
    t.sections.iter().flat_map(|s| s.rows.iter()).nth(index)
}

/// The LAYOUT indices of the rows of `t` an element stands for, in layout order: every row but a
/// separator or a note (`Row::separator`, `Row::note`, both `sep`), which `TableView::n_rows` and so
/// `GroupSpec::len` count but no key can land on and `TableView::row_frame` never places. With a
/// row-key map (`keys`, a form-backed table) a row is an element only when the form binds a key to
/// it.
pub fn table_elems<'a>(
    t: &'a super::table::TableView,
    keys: Option<&'a dyn super::form::RowKeys>,
) -> impl Iterator<Item = usize> + 'a {
    t.sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .enumerate()
        .filter(move |(i, r)| !r.sep && keys.map_or(true, |k| k.key_at(*i).is_some()))
        .map(|(i, _)| i)
}

/// **The one convention for the id of a table row**, shared by [`Table`], `table_screen::TablePart`
/// and every screen that answers the hooks over a `FormTable` itself (the Source and Version
/// pickers' panels): a row is `row.<n>`, where `n` is the row's ORDINAL among the
/// table's element rows ([`table_elems`]), so a separator or a note never leaves a gap
/// (`row.0`, `row.1`, `row.2`) and an id never depends on how many dividers the layout carries. A
/// row whose form id names itself ([`FormId::a11y`](super::form::FormId::a11y), read through
/// `keys`) is that single name instead, and then answers only to it.
///
/// A toggle row is a `Toggle`, every other row a `Row`, and the label is the row's own text,
/// borrowed from the table. `index` is the LAYOUT index; a separator or note, or an index past the
/// end, is no element (`None`).
pub fn table_a11y<'t>(
    t: &'t super::table::TableView,
    keys: Option<&'t dyn super::form::RowKeys>,
    index: usize,
) -> Option<A11y<'t>> {
    let row = table_row(t, index).filter(|r| !r.sep)?;
    let path = match keys.and_then(|k| k.a11y_name(index)) {
        Some(name) => A11yPath::new().name(name),
        None => A11yPath::new().name("row").index(table_elems(t, keys).position(|i| i == index)?),
    };
    let role = if row.toggle.is_some() { Role::Toggle } else { Role::Row };
    let a = A11y::new(path, role);
    Some(if row.label.is_empty() { a } else { a.with_label(row.label.as_str()) })
}

/// The inverse of [`table_a11y`]: the LAYOUT index the relative `path` names. `row.<n>` names the
/// `n`th element row, named rows counted, and answers only when that row has no name of its own; a
/// named row answers to its name alone. A path that names no element (an ordinal past the last, a
/// name no row declares) is `None`.
pub fn table_a11y_find(
    t: &super::table::TableView,
    keys: Option<&dyn super::form::RowKeys>,
    path: &A11yPath,
) -> Option<usize> {
    let named = |i: usize| keys.and_then(|k| k.a11y_name(i));
    match (indexed(path, "row", usize::MAX), sole_name(path)) {
        // a row with a name answers only to it, so one element has one spelling
        (Some(n), _) => table_elems(t, keys).nth(n).filter(|&i| named(i).is_none()),
        (None, Some(name)) => table_elems(t, keys).find(|&i| named(i) == Some(name)),
        (None, None) => None,
    }
}

/// One shelf (`CardRow`) as the draw sees it this frame: the same arguments the tile draw takes,
/// so `place` and the drawn tile are one formula.
pub struct Shelf<'a> {
    pub(crate) row: &'a card_row::CardRow,
    pub(crate) n: usize,
    pub(crate) sty: &'a card_row::RowStyle,
    pub(crate) row_y: f32,
    pub(crate) size: (f32, f32),
    pub(crate) pitch: f32,
    pub(crate) group: GroupId,
    pub(crate) entry: EntryId,
    /// The row's horizontal extent (the panel width for a full-bleed shelf).
    pub(crate) extent: Rect,
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
            elem: ElemKind::Card,
        });
    }
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        ((key.index()? as usize) < self.n).then_some(self.group)
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
            index: Some(i as u32),
        })
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        clamp_index(self.entry, want, self.n)
    }
    fn seat(&self, _g: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let cx_ = from.rect.x + from.rect.w * 0.5;
        let guess = ((cx_ - self.sty.margin_x + self.row.scroll_x()) / self.pitch).max(0.0) as usize;
        let from_i = from.index.map_or(guess, |i| i as usize);
        let i = card_row::column_near_x(cx_, self.sty.margin_x, self.pitch, self.size.0, self.row.scroll_x(), self.n, from_i);
        key(self.entry, i)
    }
    /// `card.<i>`: a shelf holds cards, and a card is known by its position in the row. The text a
    /// card shows (its title, its ratingKey) is the item source's, which the owning screen adds.
    fn a11y(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<A11y<'_>> {
        let i = key.index()? as usize;
        (i < self.n).then(|| A11y::new(A11yPath::new().name("card").index(i), Role::Card))
    }
    fn a11y_elems(&self, g: GroupId, _cx: &Cx<'_, H>, out: &mut Vec<H::Elem>) {
        if g == self.group {
            out.extend((0..self.n as u32).map(H::Elem::of_index));
        }
    }
    fn a11y_find(&self, path: &A11yPath, _cx: &Cx<'_, H>) -> Option<H::Elem> {
        indexed(path, "card", self.n).map(|i| H::Elem::of_index(i as u32))
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
            elem: ElemKind::Control,
        });
    }
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        (key.index()? < self.table.n_rows().max(0) as u32).then_some(self.group)
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
        let i = key.index()?;
        let r = self.table.row_frame(self.frame, i as i32)?;
        Some(Placed {
            rect: r,
            rest_rect: r,
            clip: self.frame,
            index: Some(i),
        })
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let i = want.elem.index().filter(|&i| i < NAMESPACE_FLOOR).unwrap_or(0) as i32;
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
    /// `row.<n>` for each row a key can land on, `n` its ordinal among them (see [`table_a11y`]): a
    /// separator or note is a cell `GroupSpec::len` counts (`n_rows`) but no element, so the
    /// listing is shorter than it by exactly those, and `place` (`TableView::row_frame`) places
    /// every row that is listed.
    fn a11y(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<A11y<'_>> {
        table_a11y(self.table, None, key.index()? as usize)
    }
    fn a11y_elems(&self, g: GroupId, _cx: &Cx<'_, H>, out: &mut Vec<H::Elem>) {
        if g == self.group {
            out.extend(table_elems(self.table, None).map(|i| H::Elem::of_index(i as u32)));
        }
    }
    fn a11y_find(&self, path: &A11yPath, _cx: &Cx<'_, H>) -> Option<H::Elem> {
        table_a11y_find(self.table, None, path).map(|i| H::Elem::of_index(i as u32))
    }
}

/// The shared top strip's pills as DRAWN (their recorded rects): a `Row` group the container
/// contributes above the page's (spec §6.2). Until the pills come from `Cx`, the rects are read
/// off the last draw (`StripRender::pill_rects`, owned by `app::bridge::Bridge`).
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
            elem: ElemKind::Control,
        });
    }
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        ((key.index()? as usize) < self.rects.len()).then_some(self.group)
    }
    fn neighbour(&self, k: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        step_index(self.entry, k, dir, self.rects.len(), true)
    }
    fn place(&self, key: &H::Elem, _cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        let i = key.index()?;
        let r = *self.rects.get(i as usize)?;
        Some(Placed {
            rect: r,
            rest_rect: r,
            clip: Rect::FULL,
            index: Some(i),
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
    /// `tab.<i>`, by the pill's position in the strip. The container that owns the strip
    /// (`PageWithStrip`) names its pills by stable id instead (`nav.watchlist`); this is the bare
    /// widget's answer.
    fn a11y(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<A11y<'_>> {
        let i = key.index()? as usize;
        (i < self.rects.len()).then(|| A11y::new(A11yPath::new().name("tab").index(i), Role::Tab))
    }
    fn a11y_elems(&self, g: GroupId, _cx: &Cx<'_, H>, out: &mut Vec<H::Elem>) {
        if g == self.group {
            out.extend((0..self.rects.len() as u32).map(H::Elem::of_index));
        }
    }
    fn a11y_find(&self, path: &A11yPath, _cx: &Cx<'_, H>) -> Option<H::Elem> {
        indexed(path, "tab", self.rects.len()).map(|i| H::Elem::of_index(i as u32))
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
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        let (r, c) = self.split(key.index()?);
        let s = self.shelves.get(r)?;
        (c < s.n).then_some(s.group)
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
        let k = want.elem.index().filter(|&k| k < NAMESPACE_FLOOR).unwrap_or(0);
        let (r, c) = self.split(k);
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
    /// `shelf.<r>.card.<c>`: the shelf's own `card.<c>` under the position of the shelf in the
    /// column, so the shelves of one grid never meet.
    fn a11y(&self, key: &H::Elem, cx: &Cx<'_, H>) -> Option<A11y<'_>> {
        let (r, c) = self.split(key.index()?);
        let s = self.shelves.get(r)?;
        let mut a = Focusable::<H>::a11y(s, &H::Elem::of_index(c as u32), cx)?;
        a.path = A11yPath::new().name("shelf").index(r).join(&a.path);
        Some(a)
    }
    fn a11y_elems(&self, g: GroupId, _cx: &Cx<'_, H>, out: &mut Vec<H::Elem>) {
        if let Some((r, s)) = self.shelves.iter().enumerate().find(|(_, s)| s.group == g) {
            out.extend((0..s.n).map(|c| H::Elem::of_index(self.join(r, c))));
        }
    }
    fn a11y_find(&self, path: &A11yPath, cx: &Cx<'_, H>) -> Option<H::Elem> {
        if path.ns().is_some() {
            return None;
        }
        let mut segs = path.segs();
        let (Some(Seg::Name("shelf")), Some(Seg::Index(r))) = (segs.next(), segs.next()) else {
            return None;
        };
        let s = self.shelves.get(r)?;
        // the shelf answers its own `card.<c>`; this grid only knows where the shelf stands
        let c = Focusable::<H>::a11y_find(s, &path.tail(2), cx)?.index()?;
        Some(H::Elem::of_index(self.join(r, c as usize)))
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
            elem: ElemKind::Control,
        });
    }
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        (key.index()? == 0).then_some(self.group)
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
            index: Some(0),
        })
    }
    fn reconcile(&self, _want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        key(self.entry, 0)
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        key(self.entry, 0)
    }
    /// `document`: the group's one element, the page of text itself.
    fn a11y(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<A11y<'_>> {
        (key.index()? == 0).then(|| A11y::new(A11yPath::new().name("document"), Role::Text))
    }
    fn a11y_elems(&self, g: GroupId, _cx: &Cx<'_, H>, out: &mut Vec<H::Elem>) {
        if g == self.group {
            out.push(H::Elem::of_index(0));
        }
    }
    fn a11y_find(&self, path: &A11yPath, _cx: &Cx<'_, H>) -> Option<H::Elem> {
        (sole_name(path) == Some("document")).then(|| H::Elem::of_index(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{FixtureHost, FixtureMeasure, FixtureView, FixtureViews};
    use plx_machine::machine::{FocusRead, InputOwner, PressRead, Tick};

    fn cx<'a>(m: &'a FixtureMeasure, v: &'a FixtureView) -> Cx<'a, FixtureHost> {
        Cx {
            views: FixtureViews { store: v },
            tick: Tick::default(),
            measure: m,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: InputOwner::Entry(EntryId(0)),
            modal: Default::default(), page_image: Default::default(), hold_hint: Default::default(),
        }
    }

    const E: EntryId = EntryId(3);

    #[test]
    fn table_ownership_rejects_foreign_full_u32_keys() {
        use crate::table::{Row, Section, TableView};
        let (m, v) = (FixtureMeasure, FixtureView::default());
        let c = cx(&m, &v);
        let mut rows = TableView::new();
        rows.set_sections(vec![Section::new("").row(Row::new("a")).row(Row::new("b"))], 0, false);
        let table = Table { table: &rows, frame: Rect::FULL, group: GroupId(0), entry: E };
        for k in [2, 0x4000_0000, 0x8000_0000, u32::MAX] {
            assert_eq!(Focusable::<FixtureHost>::group_of(&table, &k, &c), None, "foreign key {k}");
        }
        assert_eq!(Focusable::<FixtureHost>::group_of(&table, &1, &c), Some(GroupId(0)));
    }

    /// `place` is the shelf's tile formula: the same rect `cards::Shelf` draws
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
        use crate::table::{Row, Section, TableView};
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

    /// The one reconcile rule: a stale index clamps to the last item, a key from another namespace
    /// (the strip's pill, a screen's control) re-seats on the first — never on the last by accident.
    #[test]
    fn a_foreign_key_reseats_on_the_first_item_and_a_stale_index_on_the_last() {
        assert_eq!(clamp_plain_index(2, 4), 2, "inside the range is kept");
        assert_eq!(clamp_plain_index(9, 4), 3, "a shrunk list clamps to the last survivor");
        assert_eq!(clamp_plain_index(0, 0), 0, "an empty list has only a zeroth");
        for foreign in [NAMESPACE_FLOOR, 0x2000_0004, 0x4000_0100, crate::dispatch::STRIP_BASE + 4, u32::MAX] {
            assert_eq!(clamp_plain_index(foreign, 4), 0, "{foreign:#x} is not an index");
        }
        let cx_m = FixtureMeasure;
        let v = FixtureView::default();
        let cx = cx(&cx_m, &v);
        let rects = [Rect::new(0.0, 0.0, 10.0, 10.0), Rect::new(20.0, 0.0, 10.0, 10.0), Rect::new(40.0, 0.0, 10.0, 10.0)];
        let row = TabRow { rects: &rects, group: GroupId(1), entry: E };
        let strip_pill = FocusKey { entry: E, elem: crate::dispatch::STRIP_BASE + 4 };
        assert_eq!(Focusable::<FixtureHost>::reconcile(&row, strip_pill, &cx).elem, 0);
        let stale = FocusKey { entry: E, elem: 7u32 };
        assert_eq!(Focusable::<FixtureHost>::reconcile(&row, stale, &cx).elem, 2);
    }

    fn shelf_of<'a>(r: &'a card_row::CardRow, sty: &'a card_row::RowStyle, n: usize, g: u32) -> Shelf<'a> {
        Shelf {
            row: r,
            n,
            sty,
            row_y: 100.0,
            size: (sty.w, sty.h),
            pitch: sty.w + sty.gap,
            group: GroupId(g),
            entry: E,
            extent: Rect::FULL,
        }
    }

    /// **A shelf, a grid of shelves, a tab row and a document answer the id contract** (plan A
    /// tests 1-4, through `a11y::conformance`: every element projects, the ids are unique, `a11y_find`
    /// inverts `a11y` for every element, and `a11y_elems` lists at most `GroupSpec::len`), and the
    /// ids are the documented ones: `card.<i>`, `shelf.<r>.card.<c>`, `tab.<i>`, `document`.
    #[test]
    fn the_shelf_grid_tab_row_and_document_answer_the_id_contract() {
        use crate::a11y::conformance;
        type H = FixtureHost;
        let (m, v) = (FixtureMeasure, FixtureView::default());
        let c = cx(&m, &v);
        let rel = |s: &str| A11yPath::parse_relative(s).unwrap();
        let qualified = A11yPath::parse_id("home.card.1").unwrap();
        let sty = card_row::RowStyle::HOME;

        let row = card_row::CardRow::new();
        let shelf = shelf_of(&row, &sty, 8, 1);
        conformance(&shelf, &c);
        let a = Focusable::<H>::a11y(&shelf, &3, &c).unwrap();
        assert_eq!((a.path, a.role, a.loaded), (rel("card.3"), Role::Card, true));
        assert!(Focusable::<H>::a11y(&shelf, &8, &c).is_none(), "past the end is no element");
        assert_eq!(Focusable::<H>::a11y_find(&shelf, &rel("card.7"), &c), Some(7));
        assert_eq!(Focusable::<H>::a11y_find(&shelf, &rel("card.8"), &c), None, "past the end");
        assert_eq!(Focusable::<H>::a11y_find(&shelf, &rel("row.1"), &c), None, "another widget's name");
        assert_eq!(Focusable::<H>::a11y_find(&shelf, &qualified, &c), None, "the owner strips the namespace");
        let mut elems = Vec::new();
        Focusable::<H>::a11y_elems(&shelf, GroupId(2), &c, &mut elems);
        assert!(elems.is_empty(), "a foreign group lists nothing");
        conformance(&shelf_of(&row, &sty, 0, 1), &c);

        let rows = [card_row::CardRow::new(), card_row::CardRow::new()];
        let shelves = [shelf_of(&rows[0], &sty, 10, 1), shelf_of(&rows[1], &sty, 4, 2)];
        let grid = Grid { shelves: &shelves, stride: 64, entry: E };
        conformance(&grid, &c);
        assert_eq!(Focusable::<H>::a11y(&grid, &(64 + 2), &c).unwrap().path, rel("shelf.1.card.2"));
        assert_eq!(Focusable::<H>::a11y_find(&grid, &rel("shelf.0.card.9"), &c), Some(9));
        assert_eq!(Focusable::<H>::a11y_find(&grid, &rel("shelf.1.card.3"), &c), Some(64 + 3));
        assert_eq!(Focusable::<H>::a11y_find(&grid, &rel("shelf.1.card.4"), &c), None, "the shorter shelf ends at 4");
        assert_eq!(Focusable::<H>::a11y_find(&grid, &rel("shelf.2.card.0"), &c), None, "no third shelf");
        assert_eq!(Focusable::<H>::a11y_find(&grid, &rel("card.1"), &c), None, "a bare card names no shelf");
        let mut elems = Vec::new();
        Focusable::<H>::a11y_elems(&grid, GroupId(2), &c, &mut elems);
        assert_eq!(elems, [64, 65, 66, 67], "one shelf's group is that shelf's cards, in order");

        let rects = [Rect::new(0.0, 0.0, 10.0, 10.0), Rect::new(20.0, 0.0, 10.0, 10.0), Rect::new(40.0, 0.0, 10.0, 10.0)];
        let tabs = TabRow { rects: &rects, group: GroupId(1), entry: E };
        conformance(&tabs, &c);
        assert_eq!(Focusable::<H>::a11y(&tabs, &2, &c).unwrap().role, Role::Tab);
        assert_eq!(Focusable::<H>::a11y_find(&tabs, &rel("tab.1"), &c), Some(1));
        assert_eq!(Focusable::<H>::a11y_find(&tabs, &rel("tab.3"), &c), None);

        let reader = super::super::document_reader::DocumentReader::new();
        let doc = Document { reader: &reader, frame: Rect::new(0.0, 0.0, 800.0, 600.0), group: GroupId(9), entry: E };
        conformance(&doc, &c);
        let a = Focusable::<H>::a11y(&doc, &0, &c).unwrap();
        assert_eq!((a.path, a.role), (rel("document"), Role::Text));
        assert!(Focusable::<H>::a11y(&doc, &1, &c).is_none(), "a document has the one element");
        assert_eq!(Focusable::<H>::a11y_find(&doc, &rel("document"), &c), Some(0));
        assert_eq!(Focusable::<H>::a11y_find(&doc, &rel("document.0"), &c), None);
    }

    /// **A table projects every row a key can land on, and says what each is.** `row.<n>`, `n` the
    /// row's ordinal among the rows a key can land on (the layout index counts the separator and the
    /// note, the id does not, so the ids have no gaps); a toggle row is a `Toggle`; the label of a
    /// row is its own text. A separator and a note are cells `GroupSpec::len` counts but no
    /// elements: they are not listed, not found and not projected, which is also what keeps every
    /// listed row placed (`TableView::row_frame` places neither). Rows below the viewport are found
    /// as well as the ones on screen.
    #[test]
    fn a_table_names_its_rows_and_says_which_are_not_controls() {
        use crate::a11y::conformance;
        use crate::table::{Row, Section, TableView};
        type H = FixtureHost;
        let (m, v) = (FixtureMeasure, FixtureView::default());
        let c = cx(&m, &v);
        let mut t = TableView::new();
        t.set_sections(
            vec![
                Section::new("First").row(Row::new("a")).row(Row::separator()),
                Section::new("Second").row(Row::new("b").toggle(true)).row(Row::note("careful")).row(Row::new("c")),
            ],
            0,
            false,
        );
        // a frame two rows tall: the rest are below it, and still elements
        let table = Table { table: &t, frame: Rect::new(0.0, 0.0, 600.0, 120.0), group: GroupId(5), entry: E };
        conformance(&table, &c);
        let mut groups = Vec::new();
        Focusable::<H>::groups(&table, &c, &mut groups);
        assert_eq!(groups[0].len, 5, "the group counts the separator and the note");
        let mut elems = Vec::new();
        Focusable::<H>::a11y_elems(&table, GroupId(5), &c, &mut elems);
        assert_eq!(elems, [0, 2, 4], "the listing holds the three rows a key can land on");
        let at = |i: u32| Focusable::<H>::a11y(&table, &i, &c).unwrap();
        let rel = |s: &str| A11yPath::parse_relative(s).unwrap();
        let facts: Vec<_> = elems.iter().map(|&i| (at(i).path, at(i).role, at(i).label.map(|l| l.into_owned()))).collect();
        assert_eq!(
            facts,
            [
                (rel("row.0"), Role::Row, Some("a".to_owned())),
                (rel("row.1"), Role::Toggle, Some("b".to_owned())),
                (rel("row.2"), Role::Row, Some("c".to_owned())),
            ],
            "the ordinal among the rows a key can land on: no gap where the separator and the note sit"
        );
        for sep in [1u32, 3] {
            assert!(Focusable::<H>::a11y(&table, &sep, &c).is_none(), "layout row {sep} is not an element");
            assert!(Focusable::<H>::place(&table, &sep, &c, At::SpringTarget).is_none(), "and it places nowhere");
        }
        for (ordinal, layout) in [(0, 0), (1, 2), (2, 4)] {
            assert_eq!(
                Focusable::<H>::a11y_find(&table, &rel(&format!("row.{ordinal}")), &c),
                Some(layout),
                "row.{ordinal} finds layout row {layout}, and a row below the viewport is found too"
            );
        }
        assert_eq!(Focusable::<H>::a11y_find(&table, &rel("row.3"), &c), None, "past the last element");
        assert!(Focusable::<H>::a11y(&table, &5, &c).is_none());
        let mut foreign = Vec::new();
        Focusable::<H>::a11y_elems(&table, GroupId(6), &c, &mut foreign);
        assert!(foreign.is_empty(), "a foreign group lists nothing");
    }
}
