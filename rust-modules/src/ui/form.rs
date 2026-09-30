//! **Declarative forms** — a menu or settings page described as one ordered list of rows, each
//! carrying its own identity, activation kind and action, and a [`FormTable`] that owns the
//! [`TableView`] and the row bindings together.
//!
//! WHY. Every action menu used to build its `Vec<Section>` and a PARALLEL `Vec<Action>` by hand and
//! resolve a press by indexing the second with the table's selection. The two vectors drifted (a
//! row added to one and not the other), and the raw table index doubled as the row's identity, so
//! reordering a page moved its focus keys, its remembered seats and its recorded fingerprints.
//! Here a row is declared ONCE — id, key, kind, action and presentation on one line — so moving a
//! line reorders the page and nothing else, and [`FormTable::set`] replaces the sections AND the
//! bindings in a single call so a lookup can never read a previous rebuild's bindings.
//!
//! Identity has two faces. The `Id` is the page's own enum (or a server's machine id) and is what
//! selection is restored by across rebuilds; the [`RowKey`] is the hand-assigned stable focus key
//! a page hands to the focus layer (never an enum discriminant, layout or hash). The `< ceiling`
//! bound on keys is checked here against a parameter because ui/ cannot name
//! `screens::registry::BAND` (the layer rule in ui/CLAUDE.md); `Dest` ([`RowKind::Nav`]) is likewise
//! a generic, so `SettingsPage` never enters ui/. Lookups are linear scans over the bindings —
//! picker lists reach past a hundred rows and a scan of that is cheaper than a hash; the
//! operation-count test in `form_tests.rs` pins the bound.
//!
//! Rendering is byte-identical to a hand-built `Vec<Section>`: [`Form`] produces exactly that list
//! and hands it to the existing [`TableView`] path. Design record: `docs/settings-form.md`.

// Callers land in PR 2 of the docs/settings-form.md sequence.
#![cfg_attr(not(test), allow(dead_code))]

use crate::ui::table::{Row, Section, TableView};

/// A row's stable focus key. Hand-assigned per page, never derived from layout — see the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RowKey(pub u32);

/// A page's row id type names its keys once, so call sites do not repeat them.
pub trait FormId {
    fn key(&self) -> RowKey;
}

/// What activating an item does. Presentation (chevron, knob, checkmark) stays on the [`Row`].
#[derive(Clone, Debug, PartialEq)]
pub enum RowKind<Dest> {
    /// Drill-down: activation pushes `Dest` (the caller's stack executes it).
    Nav(Dest),
    Toggle,
    Choice,
    Button,
}

/// What [`FormTable::activate`] asks the caller to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Activation<A, Dest> {
    Push(Dest),
    Action(A),
}

/// A focusable row's non-visual half, kept parallel to the table by [`FormTable`] alone.
#[derive(Clone, Debug)]
pub struct Binding<Id, A, Dest> {
    pub id: Id,
    pub key: RowKey,
    pub kind: RowKind<Dest>,
    pub action: A,
}

enum Slot<Id, A, Dest> {
    Item(Binding<Id, A, Dest>, Row),
    /// A separator or note: takes a layout index, has no id/key/action, never focusable.
    Inert(Row),
}

/// One table section: its header (and accessory/dim, carried on a row-less [`Section`]) plus slots.
pub struct FormSection<Id, A, Dest> {
    head: Section,
    visible: bool,
    slots: Vec<Slot<Id, A, Dest>>,
}
impl<Id, A, Dest> FormSection<Id, A, Dest> {
    pub fn new(header: impl Into<String>) -> Self {
        Self::from_head(Section::new(header))
    }
    /// A section whose header, accessory and dim are configured on `head` (its rows are ignored).
    pub fn from_head(mut head: Section) -> Self {
        head.rows.clear();
        Self {
            head,
            visible: true,
            slots: Vec::new(),
        }
    }
    /// `false` drops the whole section (header included) from the built table.
    pub fn visible(mut self, v: bool) -> Self {
        self.visible = v;
        self
    }
    /// A focusable row with an explicit key (dynamic rows: per-page base + position).
    pub fn item_keyed(
        mut self,
        id: Id,
        key: RowKey,
        kind: RowKind<Dest>,
        action: A,
        row: Row,
    ) -> Self {
        self.slots.push(Slot::Item(
            Binding {
                id,
                key,
                kind,
                action,
            },
            row,
        ));
        self
    }
    pub fn item_keyed_if(
        self,
        cond: bool,
        id: Id,
        key: RowKey,
        kind: RowKind<Dest>,
        action: A,
        row: Row,
    ) -> Self {
        if cond {
            self.item_keyed(id, key, kind, action, row)
        } else {
            self
        }
    }
    /// The grouping hairline: consumes a layout index, never focusable, never bound.
    pub fn separator(mut self) -> Self {
        self.slots.push(Slot::Inert(Row::separator()));
        self
    }
    /// A one-line informational note: consumes a layout index, never focusable, never bound.
    pub fn note(mut self, text: impl Into<String>) -> Self {
        self.slots.push(Slot::Inert(Row::note(text)));
        self
    }
}
impl<Id: FormId, A, Dest> FormSection<Id, A, Dest> {
    /// A focusable row; its key comes from [`FormId::key`].
    pub fn item(self, id: Id, kind: RowKind<Dest>, action: A, row: Row) -> Self {
        let key = id.key();
        self.item_keyed(id, key, kind, action, row)
    }
    /// [`Self::item`] when `cond` holds, else nothing.
    pub fn item_if(self, cond: bool, id: Id, kind: RowKind<Dest>, action: A, row: Row) -> Self {
        if cond {
            self.item(id, kind, action, row)
        } else {
            self
        }
    }
}

/// An ordered list of sections, built by a PURE function of plain inputs.
pub struct Form<Id, A, Dest> {
    sections: Vec<FormSection<Id, A, Dest>>,
}
impl<Id, A, Dest> Default for Form<Id, A, Dest> {
    fn default() -> Self {
        Self::new()
    }
}
impl<Id, A, Dest> Form<Id, A, Dest> {
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
        }
    }
    pub fn section(mut self, s: FormSection<Id, A, Dest>) -> Self {
        self.sections.push(s);
        self
    }
    /// Exactly the `Vec<Section>` a hand builder would make, and the bindings indexed by GLOBAL
    /// row index (`None` at inert slots).
    fn build(self) -> (Vec<Section>, Vec<Option<Binding<Id, A, Dest>>>) {
        let mut sections = Vec::new();
        let mut bindings = Vec::new();
        for fs in self.sections.into_iter().filter(|s| s.visible) {
            let mut sec = fs.head;
            for slot in fs.slots {
                match slot {
                    Slot::Item(b, row) => {
                        sec.rows.push(row);
                        bindings.push(Some(b));
                    }
                    Slot::Inert(row) => {
                        sec.rows.push(row);
                        bindings.push(None);
                    }
                }
            }
            sections.push(sec);
        }
        (sections, bindings)
    }
}

/// A [`TableView`] and its row bindings, replaced together by [`FormTable::set`].
pub struct FormTable<Id, A, Dest> {
    pub table: TableView,
    bindings: Vec<Option<Binding<Id, A, Dest>>>,
    key_ceiling: u32,
}
impl<Id: PartialEq + Clone, A: Clone, Dest: Clone> FormTable<Id, A, Dest> {
    /// `key_ceiling`: every [`RowKey`] must be below it (the page's key band; debug-asserted).
    pub const fn new(key_ceiling: u32) -> Self {
        Self {
            table: TableView::new(),
            bindings: Vec::new(),
            key_ceiling,
        }
    }

    /// Replace the table's sections AND the bindings in one step.
    ///
    /// Selection: `keep` present in the new form keeps its row. A `keep` that vanished lands on the
    /// nearest surviving selectable row in the OLD order (the next one after it, else the previous).
    /// `keep == None` (or nothing survives) opens on [`TableView::opening_row`], so a menu never
    /// starts on a destructive row.
    pub fn set(&mut self, form: Form<Id, A, Dest>, keep: Option<&Id>) {
        let (sections, bindings) = form.build();
        #[cfg(debug_assertions)]
        {
            let items: Vec<&Binding<Id, A, Dest>> = bindings.iter().flatten().collect();
            for (i, a) in items.iter().enumerate() {
                assert!(
                    a.key.0 < self.key_ceiling,
                    "form: RowKey({}) is not below the key ceiling {}",
                    a.key.0,
                    self.key_ceiling
                );
                for b in &items[i + 1..] {
                    assert!(a.id != b.id, "form: duplicate row Id in one set()");
                    assert!(a.key != b.key, "form: duplicate RowKey({}) in one set()", a.key.0);
                }
            }
        }
        let landing = keep.and_then(|k| self.landing_for(k, &bindings));
        self.bindings = bindings;
        match landing {
            Some(i) => self.table.set_sections(sections, i as i32, true),
            None => self.table.open_sections(sections),
        }
    }

    /// The new index `keep` lands on, or `None` when nothing (keep or a neighbour) survives.
    fn landing_for(&self, keep: &Id, new: &[Option<Binding<Id, A, Dest>>]) -> Option<usize> {
        let find = |id: &Id| new.iter().position(|b| b.as_ref().is_some_and(|b| &b.id == id));
        if let Some(i) = find(keep) {
            return Some(i);
        }
        let old_at = self.index_of(keep)?;
        let survivor = |j: usize| self.bindings[j].as_ref().and_then(|b| find(&b.id));
        (old_at + 1..self.bindings.len())
            .find_map(survivor)
            .or_else(|| (0..old_at).rev().find_map(survivor))
    }

    fn binding(&self, index: usize) -> Option<&Binding<Id, A, Dest>> {
        self.bindings.get(index)?.as_ref()
    }
    pub fn binding_at(&self, index: usize) -> Option<&Binding<Id, A, Dest>> {
        self.binding(index)
    }
    pub fn id_at(&self, index: usize) -> Option<&Id> {
        self.binding(index).map(|b| &b.id)
    }
    pub fn key_at(&self, index: usize) -> Option<RowKey> {
        self.binding(index).map(|b| b.key)
    }
    pub fn index_of(&self, id: &Id) -> Option<usize> {
        self.bindings
            .iter()
            .position(|b| b.as_ref().is_some_and(|b| &b.id == id))
    }
    pub fn index_of_key(&self, key: RowKey) -> Option<usize> {
        self.bindings
            .iter()
            .position(|b| b.as_ref().is_some_and(|b| b.key == key))
    }
    /// The id under the table's current selection.
    pub fn selected_id(&self) -> Option<&Id> {
        usize::try_from(self.table.sel)
            .ok()
            .and_then(|i| self.id_at(i))
    }
    /// What activating the row at `index` asks for: `Push(dest)` for a Nav item, else the action.
    /// `None` for an inert slot or an index off the end.
    pub fn activate(&self, index: usize) -> Option<Activation<A, Dest>> {
        let b = self.binding(index)?;
        Some(match &b.kind {
            RowKind::Nav(d) => Activation::Push(d.clone()),
            _ => Activation::Action(b.action.clone()),
        })
    }
}
