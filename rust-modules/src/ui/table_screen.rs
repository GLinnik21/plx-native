//! **The route family's screens as COMPONENTS** (restructure spec §10, phase 5a): `Header`
//! (crumb → title → copy, the narrative column), `TableScreen` (a header beside a `TableView`
//! in the content column — Settings' root, the Legal index) and `DocumentScreen` (a header
//! beside a `DocumentReader` — every Legal document, Privacy, About). Each is `Composed` of
//! `Part`s, so it answers the focus protocol (spec §7.1) through `composed_*` and draws through
//! `composed_draw`; and each also draws for the LEGACY loop through `paint(Painter)`, the same
//! routine, so the pixels are one function whichever loop calls it.
//!
//! Phase 5a EXTRACTS: the words, the geometry (`RouteLayout`) and the widgets stay where they
//! were — `ui/settings.rs` and `ui/legal.rs` keep their statics and their `RouteFocus` ladders
//! — and their draws build one of these views over that state and call `paint`. What the
//! components ADD is the focus protocol as data (spec §10's reading of route_screen's rules):
//! the table is a `Column` group with `Seat::Remembered` (rules 1, 3, 5), its LEFT edge is
//! `EdgeRule::Nav(Back)` (rule 9 — the crumb is literal) unless the screen holds an uncommitted
//! edit, in which case it is `Stop`; its RIGHT edge is `EdgeRule::Screen`, so a chevron row's
//! RIGHT reaches the screen's own `step` (rule 8) and a plain row's is dropped; a document is a
//! `Document` group that scrolls inside and leaves at its ends. The band (rules 2, 4, 6, 7) is
//! the screen's `ActionRow` and joins these views in 5b with the first owned screen; until then
//! `TableScreen::band` is `None` for both extracted screens, which draw no band.
//!
//! The Focusable half is exercised by the host tests below (spec §13: "its Focusable impl is
//! exercised by the golden tables only until 5b"); the legacy ladders keep answering the keys.
#![allow(dead_code)] // phase 5a: the Part/Composed half has no dispatcher-mounted consumer until 5b

use super::document_reader::DocumentReader;
use super::frame::Budget;
use super::geom::{Document, IndexElem, Table};
use super::machine::{Cx, EntryId, FocusKey, GroupId, Host, NavOpKind, PartId};
use super::route_screen::RouteLayout;
use super::screen::{
    composed_draw, composed_group_of, composed_groups, composed_neighbour, composed_place,
    composed_prepare, composed_reconcile, composed_seat, Activate, At, Composed, Dir, DrawFrame,
    EdgeRule, Focusable, GroupSpec, Hover, Part, Placed, Step, Stop,
};
use super::table::TableView;
use super::{theme, Painter, Rect};

/// The narrative column: crumb (where BACK goes, `None` = nowhere inside the app), title, copy.
/// Borrows its words for the frame; a screen's strings are constants or store-owned.
pub struct Header<'a> {
    pub layout: RouteLayout,
    pub crumb: Option<&'a str>,
    pub title: &'a str,
    pub copy: &'a str,
    pub copy_size: std::os::raw::c_int,
}

impl<'a> Header<'a> {
    pub fn new(layout: RouteLayout, crumb: Option<&'a str>, title: &'a str, copy: &'a str) -> Self {
        Self {
            layout,
            crumb,
            title,
            copy,
            copy_size: theme::size::LABEL,
        }
    }

    /// The one drawing routine, for both loops.
    pub fn paint(&self, p: Painter) {
        self.layout.draw_narrative(p, self.crumb, self.title, self.copy, self.copy_size);
    }
}

impl<H: Host> Focusable<H> for Header<'_> {
    fn groups(&self, _cx: &Cx<'_, H>, _out: &mut Vec<GroupSpec>) {}
    fn group_of(&self, _key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        None
    }
    fn neighbour(&self, _key: FocusKey<H::Elem>, _dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        Step::Edge
    }
    fn place(&self, _key: &H::Elem, _cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        None
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        want
    }
    fn seat(&self, _g: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        // never a destination (no group); an engine that asks anyway gets the caller's own key
        FocusKey {
            entry: EntryId(0),
            elem: unreachable_elem::<H>(from),
        }
    }
}

fn unreachable_elem<H: Host>(_from: Placed) -> H::Elem
where
{
    // A `Header` contributes no group, so the engine can never seat into it; reaching this is a
    // bug in the caller, and a panic names it rather than inventing a key.
    unreachable!("Header has no focusable element")
}

impl<H: Host> Part<H> for Header<'_> {
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, H>, _rect: Rect) {
        self.paint(f.painter);
    }
}

/// The content column as a table: a `Column` group (rules 1, 3, 5 through `Seat::Remembered`)
/// whose LEFT edge is the crumb (rule 9) and whose RIGHT edge asks the screen (rule 8).
pub struct TablePart<'a> {
    pub table: &'a mut TableView,
    pub frame: Rect,
    pub group: GroupId,
    pub entry: EntryId,
    /// Rule 9's guard: `true` while the screen holds an edit BACK would discard — LEFT is a wall.
    pub uncommitted: bool,
}

impl TablePart<'_> {
    fn view(&self) -> Table<'_> {
        Table {
            table: self.table,
            frame: self.frame,
            group: self.group,
            entry: self.entry,
        }
    }

    pub fn paint(&self, p: Painter) {
        self.table.draw(p, self.frame);
    }
}

impl<H: Host> Focusable<H> for TablePart<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let n = out.len();
        Focusable::<H>::groups(&self.view(), cx, out);
        if let Some(g) = out.get_mut(n) {
            // [up, down, left, right]: up/down are the column's own (Geometric past the ends,
            // which is how DOWN off the last row reaches a band beneath — rule 2); LEFT is BACK
            // unless an edit is at stake (rule 9); RIGHT is the screen's to answer (rule 8).
            g.edge[2] = if self.uncommitted { EdgeRule::Stop } else { EdgeRule::Nav(NavOpKind::Back) };
            g.edge[3] = EdgeRule::Screen;
        }
    }
    fn group_of(&self, key: &H::Elem, cx: &Cx<'_, H>) -> Option<GroupId> {
        Focusable::<H>::group_of(&self.view(), key, cx)
    }
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, cx: &Cx<'_, H>) -> Step<H::Elem> {
        Focusable::<H>::neighbour(&self.view(), key, dir, cx)
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        Focusable::<H>::place(&self.view(), key, cx, at)
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        Focusable::<H>::reconcile(&self.view(), want, cx)
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        Focusable::<H>::seat(&self.view(), g, from, cx)
    }
}

impl<H: Host> Part<H> for TablePart<'_>
where
    H::Elem: IndexElem,
{
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, H>, rect: Rect) {
        let p = f.painter;
        self.frame = rect;
        self.paint(p);
        // every selectable row is a stop (rule 11: hover parks, a click activates)
        for i in 0..self.table.n_rows() {
            if self.table.next_selectable(i, 0) != Some(i) {
                continue;
            }
            if let Some(r) = self.table.row_frame(rect, i) {
                f.stop(
                    p,
                    Stop {
                        key: FocusKey {
                            entry: self.entry,
                            elem: H::Elem::of_index(i as u32),
                        },
                        rect: r,
                        rest_rect: r,
                        clip: rect,
                        hover: Hover::Focus,
                        activate: Activate::Press,
                    },
                );
            }
        }
    }
}

/// A header beside a table — the Settings root and the Legal index (spec §10 `TableScreen`).
pub struct TableScreen<'a> {
    pub header: Header<'a>,
    pub table: TablePart<'a>,
}

impl<'a> TableScreen<'a> {
    /// Over a route's own layout: the table in the content column's SECTIONED frame.
    pub fn new(header: Header<'a>, table: &'a mut TableView, group: GroupId, entry: EntryId) -> Self {
        let frame = header.layout.sectioned_table();
        Self {
            header,
            table: TablePart {
                table,
                frame,
                group,
                entry,
                uncommitted: false,
            },
        }
    }

    pub fn uncommitted(mut self, v: bool) -> Self {
        self.table.uncommitted = v;
        self
    }

    /// The legacy loop's draw: header, then table.
    pub fn paint(&self, p: Painter) {
        self.header.paint(p);
        self.table.paint(p);
    }
}

impl<H: Host> Composed<H> for TableScreen<'_>
where
    H::Elem: IndexElem,
{
    fn layout(&self, _cx: &Cx<'_, H>) -> Vec<(PartId, Rect)> {
        vec![(PartId(0), self.header.layout.narrative), (PartId(1), self.table.frame)]
    }
    fn part(&self, id: PartId) -> &dyn Part<H> {
        match id {
            PartId(0) => &self.header,
            _ => &self.table,
        }
    }
    fn part_mut(&mut self, id: PartId) -> &mut dyn Part<H> {
        match id {
            PartId(0) => &mut self.header,
            _ => &mut self.table,
        }
    }
}

impl<H: Host> Focusable<H> for TableScreen<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        composed_groups(self, cx, out)
    }
    fn group_of(&self, key: &H::Elem, cx: &Cx<'_, H>) -> Option<GroupId> {
        composed_group_of(self, key, cx)
    }
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, cx: &Cx<'_, H>) -> Step<H::Elem> {
        composed_neighbour(self, key, dir, cx)
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        composed_place(self, key, cx, at)
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        composed_reconcile(self, want, cx)
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        composed_seat(self, g, from, cx)
    }
}

impl<H: Host> Part<H> for TableScreen<'_>
where
    H::Elem: IndexElem,
{
    fn prepare(&mut self, b: &mut Budget, cx: &Cx<'_, H>) {
        composed_prepare(self, b, cx)
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, H>, _rect: Rect) {
        composed_draw(self, f)
    }
}

/// The content column as a document: ONE element that scrolls inside and leaves at its ends.
pub struct DocumentPart<'a> {
    pub reader: &'a mut DocumentReader,
    pub frame: Rect,
    pub body: &'a str,
    pub group: GroupId,
    pub entry: EntryId,
}

impl DocumentPart<'_> {
    fn view(&self) -> Document<'_> {
        Document {
            reader: self.reader,
            frame: self.frame,
            group: self.group,
            entry: self.entry,
        }
    }

    pub fn paint(&mut self, p: Painter) {
        self.reader.draw(p, self.frame, None, self.body);
    }
}

impl<H: Host> Focusable<H> for DocumentPart<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let n = out.len();
        Focusable::<H>::groups(&self.view(), cx, out);
        if let Some(g) = out.get_mut(n) {
            // a document is a READ: LEFT always walks back out (rule 9 with nothing to lose)
            g.edge[2] = EdgeRule::Nav(NavOpKind::Back);
        }
    }
    fn group_of(&self, key: &H::Elem, cx: &Cx<'_, H>) -> Option<GroupId> {
        Focusable::<H>::group_of(&self.view(), key, cx)
    }
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, cx: &Cx<'_, H>) -> Step<H::Elem> {
        Focusable::<H>::neighbour(&self.view(), key, dir, cx)
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        Focusable::<H>::place(&self.view(), key, cx, at)
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        Focusable::<H>::reconcile(&self.view(), want, cx)
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        Focusable::<H>::seat(&self.view(), g, from, cx)
    }
}

impl<H: Host> Part<H> for DocumentPart<'_>
where
    H::Elem: IndexElem,
{
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, H>, rect: Rect) {
        let p = f.painter;
        self.frame = rect;
        self.paint(p);
        f.stop(
            p,
            Stop {
                key: FocusKey {
                    entry: self.entry,
                    elem: H::Elem::of_index(0),
                },
                rect,
                rest_rect: rect,
                clip: rect,
                hover: Hover::Ignore,
                activate: Activate::Direct,
            },
        );
    }
}

/// A header beside a document — every Legal document, Privacy, About (spec §10
/// `DocumentScreen`).
pub struct DocumentScreen<'a> {
    pub header: Header<'a>,
    pub doc: DocumentPart<'a>,
}

impl<'a> DocumentScreen<'a> {
    /// Over a route's own layout: the document hangs from the TITLE's anchor (`RouteLayout::document`).
    pub fn new(header: Header<'a>, reader: &'a mut DocumentReader, body: &'a str, group: GroupId, entry: EntryId) -> Self {
        let frame = header.layout.document(header.crumb.is_some());
        Self {
            header,
            doc: DocumentPart {
                reader,
                frame,
                body,
                group,
                entry,
            },
        }
    }

    pub fn paint(&mut self, p: Painter) {
        self.header.paint(p);
        self.doc.paint(p);
    }
}

impl<H: Host> Composed<H> for DocumentScreen<'_>
where
    H::Elem: IndexElem,
{
    fn layout(&self, _cx: &Cx<'_, H>) -> Vec<(PartId, Rect)> {
        vec![(PartId(0), self.header.layout.narrative), (PartId(1), self.doc.frame)]
    }
    fn part(&self, id: PartId) -> &dyn Part<H> {
        match id {
            PartId(0) => &self.header,
            _ => &self.doc,
        }
    }
    fn part_mut(&mut self, id: PartId) -> &mut dyn Part<H> {
        match id {
            PartId(0) => &mut self.header,
            _ => &mut self.doc,
        }
    }
}

impl<H: Host> Focusable<H> for DocumentScreen<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        composed_groups(self, cx, out)
    }
    fn group_of(&self, key: &H::Elem, cx: &Cx<'_, H>) -> Option<GroupId> {
        composed_group_of(self, key, cx)
    }
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, cx: &Cx<'_, H>) -> Step<H::Elem> {
        composed_neighbour(self, key, dir, cx)
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        composed_place(self, key, cx, at)
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        composed_reconcile(self, want, cx)
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        composed_seat(self, g, from, cx)
    }
}

impl<H: Host> Part<H> for DocumentScreen<'_>
where
    H::Elem: IndexElem,
{
    fn prepare(&mut self, b: &mut Budget, cx: &Cx<'_, H>) {
        composed_prepare(self, b, cx)
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, H>, _rect: Rect) {
        composed_draw(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixture::{FixtureHost, FixtureMeasure, FixtureView, FixtureViews};
    use crate::ui::machine::{FocusRead, InputOwner, PressRead, Tick};
    use crate::ui::screen::{GroupKind, Seat};
    use crate::ui::table::{Row, Section};

    // The PARTS are asked directly rather than through `Composed` (`composed_groups` and its
    // siblings): `part()` materialises a `&dyn Part` vtable, whose `draw` reaches `TextView` and
    // SDL2_ttf — which the host suite cannot LINK (`ui/CLAUDE.md`'s boundary). The composed fns
    // are the fixture screen's subject already.
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

    const E: EntryId = EntryId(5);
    const G: GroupId = GroupId(9);

    fn same(a: Rect, b: Rect) -> bool {
        (a.x, a.y, a.w, a.h) == (b.x, b.y, b.w, b.h)
    }

    fn table(rows: usize) -> TableView {
        let mut t = TableView::new();
        let mut s = Section::new("Section");
        for i in 0..rows {
            s = s.row(Row::new(format!("row {i}")).chevron(i == 0));
        }
        t.set_sections(vec![s], 0, false);
        t
    }

    /// Spec §10: the table screen's focus protocol AS DATA — one Column group, Seat::Remembered,
    /// LEFT = BACK (rule 9) and RIGHT = the screen's (rule 8); with an uncommitted edit LEFT is a
    /// wall; the header contributes no group.
    #[test]
    fn a_table_screen_is_one_remembered_column_whose_left_edge_is_back() {
        let (m, v) = (FixtureMeasure, FixtureView::default());
        let cx = cx(&m, &v);
        let mut t = table(3);
        let layout = RouteLayout::screen();
        let ts = TableScreen::new(Header::new(layout, Some("Settings"), "Legal notices", "copy"), &mut t, G, E);
        let mut groups = Vec::new();
        Focusable::<FixtureHost>::groups(&ts.header, &cx, &mut groups);
        assert!(groups.is_empty(), "the header is not a group");
        Focusable::<FixtureHost>::groups(&ts.table, &cx, &mut groups);
        assert_eq!(groups.len(), 1);
        let g = groups[0];
        assert_eq!(g.id, G);
        assert!(matches!(g.kind, GroupKind::Column));
        assert_eq!(g.seat, Seat::Remembered);
        assert_eq!(g.len, 3);
        assert_eq!(g.edge[2], EdgeRule::Nav(NavOpKind::Back), "LEFT follows the crumb");
        assert_eq!(g.edge[3], EdgeRule::Screen, "RIGHT on a chevron row is the screen's");
        assert!(same(g.extent, layout.sectioned_table()));
        let mut t2 = table(3);
        let dirty = TableScreen::new(Header::new(layout, None, "Privacy", "copy"), &mut t2, G, E).uncommitted(true);
        let mut groups = Vec::new();
        Focusable::<FixtureHost>::groups(&dirty.table, &cx, &mut groups);
        assert_eq!(groups[0].edge[2], EdgeRule::Stop, "an edit at stake makes LEFT a wall");
    }

    /// Spec §7.1: geometry IS `place` — a row's placement is the rect `TableView` draws it at,
    /// and DOWN walks the rows to an edge.
    #[test]
    fn a_table_screen_places_rows_where_the_table_draws_them() {
        let (m, v) = (FixtureMeasure, FixtureView::default());
        let cx = cx(&m, &v);
        let mut t = table(3);
        let layout = RouteLayout::screen();
        let frame = layout.sectioned_table();
        let ts = TableScreen::new(Header::new(layout, None, "Settings", "copy"), &mut t, G, E);
        for i in 0..3u32 {
            let placed = Focusable::<FixtureHost>::place(&ts.table, &i, &cx, At::SpringTarget).expect("placed");
            assert!(same(placed.rect, ts.table.table.row_frame(frame, i as i32).unwrap()));
            assert_eq!(placed.index, Some(i));
        }
        assert!(Focusable::<FixtureHost>::place(&ts.table, &3, &cx, At::SpringTarget).is_none());
        let k = |i: u32| FocusKey { entry: E, elem: i };
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&ts.table, k(0), Dir::Down, &cx), Step::Move(m) if m.elem == 1));
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&ts.table, k(2), Dir::Down, &cx), Step::Edge));
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&ts.table, k(1), Dir::Left, &cx), Step::Edge), "LEFT is an edge: the rule is on the group");
    }

    /// Spec §10: a document screen is one `Document` group that scrolls inside and leaves at its
    /// ends, its LEFT edge BACK, hanging from the title anchor.
    #[test]
    fn a_document_screen_is_one_document_group_that_leaves_at_its_ends() {
        let (m, v) = (FixtureMeasure, FixtureView::default());
        let cx = cx(&m, &v);
        let mut r = DocumentReader::new();
        r.set_extent_for_test(400.0);
        let layout = RouteLayout::screen();
        let ds = DocumentScreen::new(Header::new(layout, Some("Legal notices"), "Privacy", "copy"), &mut r, "body", G, E);
        let mut groups = Vec::new();
        Focusable::<FixtureHost>::groups(&ds.doc, &cx, &mut groups);
        assert_eq!(groups.len(), 1);
        assert!(matches!(groups[0].kind, GroupKind::Document));
        assert_eq!(groups[0].edge[2], EdgeRule::Nav(NavOpKind::Back));
        assert!(same(groups[0].extent, layout.document(true)));
        let k = FocusKey { entry: E, elem: 0u32 };
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&ds.doc, k, Dir::Up, &cx), Step::Edge), "at the top, UP leaves");
        assert!(matches!(Focusable::<FixtureHost>::neighbour(&ds.doc, k, Dir::Down, &cx), Step::Move(_)), "not at the end: DOWN scrolls inside");
        assert!(same(Focusable::<FixtureHost>::place(&ds.doc, &0, &cx, At::Drawn).unwrap().rect, layout.document(true)));
    }
}
