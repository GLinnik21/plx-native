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
//! Three more things a declared row carries for pages that drill in (the player's track menu, and
//! Settings pages 3-5 of the sequence): a [`RowKind::Nav`] row gets the drill-in chevron when it is
//! built; a [`RowKind::Choice`] row declared with [`FormSection::choice`] derives its checkmark from
//! a current-value predicate instead of a hand-set flag; and an item can be
//! [`FormSection::disabled`] — drawn dim, still focusable (the viewer can land on it and read why),
//! and never activated by [`FormTable::activate`], so OK and RIGHT cannot act on it and no caller
//! carries its own guard. A table is (re)installed by one of FOUR operations — [`FormTable::set`]
//! (Settings' snap), [`FormTable::open`], [`FormTable::refresh`], [`FormTable::restore`] — because
//! "the page changed", "the same page's data changed" and "a page came back" want different scroll
//! and pill behaviour. Design record: `docs/player-submenus.md`.
//!
//! Rendering is byte-identical to a hand-built `Vec<Section>`: [`Form`] produces exactly that list
//! and hands it to the existing [`TableView`] path. Design record: `docs/settings-form.md`.

// The Settings root is the first caller (PR 2 of the docs/settings-form.md sequence); the note,
// separator, keyed-if and picker-facing halves land with their pages in PR 3-5.
#![cfg_attr(not(test), allow(dead_code))]

use crate::ui::table::{Row, Section, TableView};

/// A row's stable focus key. Hand-assigned per page, never derived from layout — see the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RowKey(pub u32);

/// A table's row-key map, as the focus layer reads it: `TablePart` translates between the focus
/// key an element carries (a [`RowKey`]'s number) and the row index the [`TableView`] draws.
/// [`FormTable`] implements it; a table with no form has no map and keeps index elements.
pub trait RowKeys {
    fn key_at(&self, index: usize) -> Option<RowKey>;
    fn index_of_key(&self, key: RowKey) -> Option<usize>;
    /// The key the focus engine must be moved to because a rebuild landed the page on a different
    /// row than the one the engine's last key names; `None` when they agree. The page's identity
    /// landing wins over the engine's key until the engine's next `FocusMoved` (see
    /// [`FormTable::note_engine_key`]).
    fn reseat(&self) -> Option<RowKey>;
}

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
    /// Set by [`FormSection::disabled`]: [`FormTable::activate`] answers `None`.
    pub disabled: bool,
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
    /// The last declaration was an `_if` that held nothing back: a trailing [`Self::disabled`]
    /// then refers to an item that does not exist and must not fall onto the one before it.
    skipped: bool,
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
            skipped: false,
        }
    }
    /// The one place a slot is appended, so `skipped` cannot outlive the declaration it describes.
    fn push(&mut self, slot: Slot<Id, A, Dest>) {
        self.skipped = false;
        self.slots.push(slot);
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
        // a drill-in row always reads as one: the chevron comes from the kind, never a second call
        // (an explicit trailing icon the caller chose stays)
        let row = match (&kind, row.ticon) {
            (RowKind::Nav(_), None) => row.chevron(true),
            _ => row,
        };
        self.push(Slot::Item(
            Binding {
                id,
                key,
                kind,
                action,
                disabled: false,
            },
            row,
        ));
        self
    }
    /// Mark the item just declared DISABLED when `cond` holds: drawn dim ([`Row::dim`]), still
    /// focusable, and never activated ([`FormTable::activate`] answers `None` for OK and RIGHT
    /// alike). A no-op when the last slot is not an item, and when the last declaration was an
    /// [`Self::item_if`] / [`Self::item_keyed_if`] that was skipped: `.item_if(false, …)
    /// .disabled(true)` ("dim it during this state") must not dim the row BEFORE the one that was
    /// never declared.
    pub fn disabled(mut self, cond: bool) -> Self {
        if let Some(Slot::Item(b, row)) = self.slots.last_mut().filter(|_| cond && !self.skipped) {
            b.disabled = true;
            row.dim = true;
        }
        self
    }
    pub fn item_keyed_if(
        mut self,
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
            self.skipped = true;
            self
        }
    }
    /// The grouping hairline: consumes a layout index, never focusable, never bound.
    pub fn separator(mut self) -> Self {
        self.push(Slot::Inert(Row::separator()));
        self
    }
    /// A one-line informational note: consumes a layout index, never focusable, never bound.
    pub fn note(mut self, text: impl Into<String>) -> Self {
        self.push(Slot::Inert(Row::note(text)));
        self
    }
}
impl<Id: FormId, A, Dest> FormSection<Id, A, Dest> {
    /// A focusable row; its key comes from [`FormId::key`].
    pub fn item(self, id: Id, kind: RowKind<Dest>, action: A, row: Row) -> Self {
        let key = id.key();
        self.item_keyed(id, key, kind, action, row)
    }
    /// A [`RowKind::Choice`] row whose checkmark is derived, not hand-set: it is checked when
    /// `is_current(&id)` holds, so a picker declares its rows once and passes the page's current
    /// value as a predicate at build.
    pub fn choice(self, id: Id, action: A, row: Row, is_current: impl Fn(&Id) -> bool) -> Self {
        let row = row.checked(is_current(&id));
        self.item(id, RowKind::Choice, action, row)
    }
    /// [`Self::item`] when `cond` holds, else nothing.
    pub fn item_if(mut self, cond: bool, id: Id, kind: RowKind<Dest>, action: A, row: Row) -> Self {
        if cond {
            self.item(id, kind, action, row)
        } else {
            self.skipped = true;
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
    /// The key the focus engine last reported holding. A rebuild can reassign it to another row
    /// (a position-keyed row moves up), so it is compared by identity after [`Self::set`].
    /// `None` while focus is off the table.
    engine_key: Option<RowKey>,
    /// Set by [`Self::set`] when the landed row is not the engine's row; cleared by
    /// [`Self::note_engine_key`].
    reseat: Option<RowKey>,
}
impl<Id: PartialEq + Clone, A: Clone, Dest: Clone> FormTable<Id, A, Dest> {
    /// `key_ceiling`: every [`RowKey`] must be below it (the page's key band; debug-asserted).
    pub const fn new(key_ceiling: u32) -> Self {
        Self {
            table: TableView::new(),
            bindings: Vec::new(),
            key_ceiling,
            engine_key: None,
            reseat: None,
        }
    }

    /// Replace the table's sections AND the bindings in one step.
    ///
    /// Selection: `keep` present in the new form keeps its row. A `keep` that vanished lands on the
    /// nearest surviving selectable row in the OLD order (the next one after it, else the previous).
    /// `keep == None` (or nothing survives) opens on [`TableView::opening_row`], so a menu never
    /// starts on a destructive row. The pill snaps and the scroll returns to the top; for a page
    /// that keeps its scroll use [`Self::refresh`], to reinstate a saved one [`Self::restore`].
    pub fn set(&mut self, form: Form<Id, A, Dest>, keep: Option<&Id>) {
        let (sections, bindings) = self.check(form);
        let landing = keep.and_then(|k| self.landing_for(k, &bindings));
        self.bindings = bindings;
        // snap, never glide: a rebuild re-derives the page, as the pre-form root did (`slide=false`)
        match landing {
            Some(i) => self.table.set_sections(sections, i as i32, false),
            None => self.table.open_sections(sections),
        }
        self.reseat_after_install();
    }

    /// **Open** a page: snap the pill, scroll to the top and focus `initial` when the form has it,
    /// else [`TableView::opening_row`] (never a destructive row). Unlike [`Self::set`] there is no
    /// old order to fall back through: a page's initial focus is an explicit id the caller chose,
    /// and a missing one is simply "no initial".
    pub fn open(&mut self, form: Form<Id, A, Dest>, initial: Option<&Id>) {
        let (sections, bindings) = self.check(form);
        let at = initial.and_then(|id| Self::position_of(&bindings, id));
        self.bindings = bindings;
        match at {
            Some(i) => self.table.set_sections(sections, i as i32, false),
            None => self.table.open_sections(sections),
        }
        self.reseat_after_install();
    }

    /// **Refresh** the page in place: the same page whose data changed (a live poll added a row).
    /// The scroll and the pill are kept, the selection is restored by the id it was on (falling
    /// back through the old order as [`Self::set`] does) and the pill SLIDES to it, so a row
    /// appearing above the viewer moves the highlight rather than teleporting it.
    pub fn refresh(&mut self, form: Form<Id, A, Dest>) {
        self.refresh_with(form, None, None);
    }

    /// [`Self::refresh`] with two explicit ids, for a page whose focus is not always where the
    /// viewer left it. Landing order: `prefer` (an id the page banked while its row was away and
    /// wants back the moment it returns), the id the table was on, `fallback` (the page's own
    /// "sensible row" when the viewer's row is gone, e.g. the checked track), then the old-order
    /// neighbour as in [`Self::set`]. Each step applies only when the new form still has that id.
    pub fn refresh_with(&mut self, form: Form<Id, A, Dest>, prefer: Option<&Id>, fallback: Option<&Id>) {
        let (sections, bindings) = self.check(form);
        let find = |id: &Id| Self::position_of(&bindings, id);
        let keep = self.selected_id().cloned();
        let landing = prefer
            .and_then(find)
            .or_else(|| keep.as_ref().and_then(find))
            .or_else(|| fallback.and_then(find))
            .or_else(|| keep.as_ref().and_then(|k| self.landing_for(k, &bindings)));
        self.bindings = bindings;
        self.table.set_sections_or_open(sections, landing.map(|i| i as i32), true);
        self.reseat_after_install();
    }

    /// **Restore** a saved view: the page returning from a drill-in. Selection snaps to `id` (else
    /// [`TableView::opening_row`]) and the scroll is put back at `scroll` (see
    /// [`TableView::scroll_pos`]), so the list comes back exactly as it was left.
    pub fn restore(&mut self, form: Form<Id, A, Dest>, id: Option<&Id>, scroll: f32) {
        let (sections, bindings) = self.check(form);
        let at = id.and_then(|id| Self::position_of(&bindings, id));
        self.bindings = bindings;
        self.table.restore_sections(sections, at.map(|i| i as i32), scroll);
        self.reseat_after_install();
    }

    fn position_of(new: &[Option<Binding<Id, A, Dest>>], id: &Id) -> Option<usize> {
        new.iter().position(|b| b.as_ref().is_some_and(|b| &b.id == id))
    }

    /// Build the form and, in debug builds, assert its ids and keys are unique and below the ceiling.
    fn check(&self, form: Form<Id, A, Dest>) -> (Vec<Section>, Vec<Option<Binding<Id, A, Dest>>>) {
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
        (sections, bindings)
    }

    /// The landing is by identity; the engine still holds the key it had. If that key no longer
    /// names the landed row (the row moved, or vanished), the landing wins.
    fn reseat_after_install(&mut self) {
        let landed = self.selected_id();
        self.reseat = self.engine_key.and_then(|held| {
            let names = self.index_of_key(held).and_then(|i| self.id_at(i));
            (names != landed).then(|| self.key_at(self.table.sel.max(0) as usize)).flatten()
        });
    }

    /// Record the key the focus engine now holds (`None`: focus is off the table — the band, an
    /// alert). Call it from the page's `FocusMoved` handler; it ends any pending [`Self::reseat`].
    pub fn note_engine_key(&mut self, key: Option<RowKey>) {
        self.engine_key = key.filter(|k| self.index_of_key(*k).is_some());
        self.reseat = None;
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
    /// `None` for an inert slot, an index off the end, or a [`FormSection::disabled`] item.
    pub fn activate(&self, index: usize) -> Option<Activation<A, Dest>> {
        let b = self.binding(index).filter(|b| !b.disabled)?;
        Some(match &b.kind {
            RowKind::Nav(d) => Activation::Push(d.clone()),
            _ => Activation::Action(b.action.clone()),
        })
    }
}

impl<Id: PartialEq + Clone, A: Clone, Dest: Clone> RowKeys for FormTable<Id, A, Dest> {
    fn key_at(&self, index: usize) -> Option<RowKey> {
        Self::key_at(self, index)
    }
    fn index_of_key(&self, key: RowKey) -> Option<usize> {
        Self::index_of_key(self, key)
    }
    fn reseat(&self) -> Option<RowKey> {
        self.reseat
    }
}
