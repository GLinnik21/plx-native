//! Unit tests for `ui/form.rs` (design record: docs/settings-form.md).

use super::form::*;
use crate::ui::table::{Row, Section};
use std::cell::Cell;

#[derive(Clone, Debug, PartialEq)]
enum Id {
    A,
    B,
    C,
    D,
    Dup,
}
impl FormId for Id {
    fn key(&self) -> RowKey {
        RowKey(match self {
            Id::A => 10,
            Id::B => 20,
            Id::C => 30,
            Id::D => 40,
            Id::Dup => 10, // collides with A on purpose
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
enum Act {
    A,
    B,
    C,
    D,
}
#[derive(Clone, Debug, PartialEq)]
enum Dest {
    Sub,
}
type T = FormTable<Id, Act, Dest>;
type S = FormSection<Id, Act, Dest>;
type F = Form<Id, Act, Dest>;

fn nav() -> RowKind<Dest> {
    RowKind::Nav(Dest::Sub)
}

/// A, B, sep, C, note, D — with A a Nav.
fn sample(show_c: bool) -> F {
    F::new()
        .section(
            S::new("First")
                .item(Id::A, nav(), Act::A, Row::new("Alpha").chevron(true))
                .item(Id::B, RowKind::Toggle, Act::B, Row::new("Beta").toggle(true)),
        )
        .section(
            S::new("Second")
                .separator()
                .item_if(show_c, Id::C, RowKind::Choice, Act::C, Row::new("Gamma"))
                .note("A note")
                .item(Id::D, RowKind::Button, Act::D, Row::new("Delta")),
        )
}

fn table(f: F) -> T {
    let mut t = T::new(100);
    t.set(f, None);
    t
}

fn shape(sections: &[Section]) -> Vec<(String, Vec<(String, bool, bool, Option<bool>, bool)>)> {
    sections
        .iter()
        .map(|s| {
            (
                s.header.clone(),
                s.rows
                    .iter()
                    .map(|r| (r.label.clone(), r.sep, r.dim, r.toggle, r.ticon.is_some()))
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn renders_exactly_the_hand_built_sections() {
    let t = table(sample(true));
    let hand = vec![
        Section::new("First")
            .row(Row::new("Alpha").chevron(true))
            .row(Row::new("Beta").toggle(true)),
        Section::new("Second")
            .row(Row::separator())
            .row(Row::new("Gamma"))
            .row(Row::note("A note"))
            .row(Row::new("Delta")),
    ];
    assert_eq!(shape(&t.table.sections), shape(&hand));
    assert_eq!(t.table.n_rows(), 6);
}

#[test]
fn id_and_key_resolution() {
    let t = table(sample(true));
    assert_eq!(t.index_of(&Id::A), Some(0));
    assert_eq!(t.index_of(&Id::D), Some(5));
    assert_eq!(t.index_of_key(RowKey(30)), Some(3));
    assert_eq!(t.id_at(1), Some(&Id::B));
    assert_eq!(t.key_at(1), Some(RowKey(20)));
    assert_eq!(t.binding_at(3).map(|b| b.action.clone()), Some(Act::C));
    assert_eq!(t.index_of_key(RowKey(99)), None);
    assert_eq!(t.id_at(99), None);
}

#[test]
fn inert_slots_are_never_bound_or_selectable() {
    let t = table(sample(true));
    for i in [2usize, 4] {
        assert!(t.binding_at(i).is_none() && t.id_at(i).is_none() && t.key_at(i).is_none());
        assert!(t.activate(i).is_none());
        assert_eq!(t.table.next_selectable(i as i32, 0), None);
    }
    assert_eq!(t.table.n_rows(), 6);
}

#[test]
fn selected_id_follows_the_table_selection() {
    let mut t = table(sample(true));
    assert_eq!(t.selected_id(), Some(&Id::A));
    t.table.sel = 3;
    assert_eq!(t.selected_id(), Some(&Id::C));
}

#[test]
fn visible_false_section_vanishes_and_item_if_drops_a_row() {
    let f = F::new()
        .section(S::new("Hidden").visible(false).item(
            Id::A,
            RowKind::Button,
            Act::A,
            Row::new("x"),
        ))
        .section(S::new("Shown").item_if(false, Id::B, RowKind::Button, Act::B, Row::new("y")))
        .section(S::new("Also").item(Id::C, RowKind::Button, Act::C, Row::new("z")));
    let t = table(f);
    assert_eq!(t.table.sections.len(), 2);
    assert_eq!(t.table.sections[0].header, "Shown");
    assert_eq!(t.table.n_rows(), 1);
    assert_eq!(t.index_of(&Id::A), None);
    assert_eq!(t.index_of(&Id::B), None);
    assert_eq!(t.index_of(&Id::C), Some(0));
}

#[test]
fn activate_pushes_for_nav_and_returns_the_action_otherwise() {
    let t = table(sample(true));
    assert_eq!(t.activate(0), Some(Activation::Push(Dest::Sub)));
    assert_eq!(t.activate(1), Some(Activation::Action(Act::B)));
    assert_eq!(t.activate(3), Some(Activation::Action(Act::C)));
    assert_eq!(t.activate(5), Some(Activation::Action(Act::D)));
    assert_eq!(t.activate(50), None);
}

#[test]
fn keep_survives_a_reorder_by_id() {
    let mut t = table(sample(true));
    t.table.sel = 5; // D
    let reordered = F::new().section(
        S::new("Only")
            .item(Id::D, RowKind::Button, Act::D, Row::new("Delta"))
            .item(Id::A, nav(), Act::A, Row::new("Alpha")),
    );
    t.set(reordered, Some(&Id::D));
    assert_eq!(t.selected_id(), Some(&Id::D));
    assert_eq!(t.table.sel, 0);
}

#[test]
fn vanished_keep_falls_to_the_next_survivor_then_the_previous() {
    // C (index 3) vanishes: the next surviving row in the old order is D.
    let mut t = table(sample(true));
    t.set(sample(true), Some(&Id::C));
    t.set(sample(false), Some(&Id::C));
    assert_eq!(t.selected_id(), Some(&Id::D));

    // D is the LAST row and vanishes: fall back to the previous survivor (C).
    let mut t = table(sample(true));
    let f = F::new()
        .section(
            S::new("First")
                .item(Id::A, nav(), Act::A, Row::new("Alpha"))
                .item(Id::B, RowKind::Toggle, Act::B, Row::new("Beta")),
        )
        .section(S::new("Second").item(Id::C, RowKind::Choice, Act::C, Row::new("Gamma")));
    t.set(f, Some(&Id::D));
    assert_eq!(t.selected_id(), Some(&Id::C));
}

#[test]
fn next_survivor_skips_other_vanished_rows() {
    let mut t = table(sample(true));
    // B and C vanish together; keep = B -> next survivor in old order is D.
    let f = F::new().section(
        S::new("x")
            .item(Id::A, nav(), Act::A, Row::new("Alpha"))
            .item(Id::D, RowKind::Button, Act::D, Row::new("Delta")),
    );
    t.set(f, Some(&Id::B));
    assert_eq!(t.selected_id(), Some(&Id::D));
}

#[test]
fn no_keep_opens_on_the_first_selectable_row() {
    let f = F::new().section(
        S::new("x")
            .separator()
            .item(Id::A, nav(), Act::A, Row::new("Alpha"))
            .item(Id::B, RowKind::Button, Act::B, Row::new("Beta")),
    );
    let t = table(f);
    assert_eq!(t.table.sel, 1);
    assert_eq!(t.selected_id(), Some(&Id::A));
}

#[test]
fn no_keep_never_opens_on_a_destructive_row() {
    let mut destructive = Row::new("Sign out");
    destructive.destructive = true;
    let f = F::new().section(
        S::new("x")
            .item(Id::A, RowKind::Button, Act::A, destructive)
            .item(Id::B, RowKind::Button, Act::B, Row::new("Beta")),
    );
    let t = table(f);
    assert_eq!(t.selected_id(), Some(&Id::B));
}

#[test]
#[should_panic(expected = "duplicate row Id")]
fn duplicate_id_asserts() {
    let f = F::new().section(
        S::new("x")
            .item(Id::A, nav(), Act::A, Row::new("1"))
            .item_keyed(Id::A, RowKey(11), nav(), Act::A, Row::new("2")),
    );
    table(f);
}

#[test]
#[should_panic(expected = "duplicate RowKey")]
fn duplicate_key_asserts() {
    let f = F::new().section(
        S::new("x")
            .item(Id::A, nav(), Act::A, Row::new("1"))
            .item(Id::Dup, nav(), Act::A, Row::new("2")),
    );
    table(f);
}

#[test]
#[should_panic(expected = "key ceiling")]
fn key_at_or_above_the_ceiling_asserts() {
    let f = F::new().section(S::new("x").item(Id::B, nav(), Act::A, Row::new("1")));
    let mut t = T::new(20); // B's key is 20: not below
    t.set(f, None);
}

thread_local! { static CMP: Cell<usize> = const { Cell::new(0) }; }

/// An id whose every comparison is counted.
#[derive(Clone, Debug)]
struct Counted(u32);
impl PartialEq for Counted {
    fn eq(&self, o: &Self) -> bool {
        CMP.with(|c| c.set(c.get() + 1));
        self.0 == o.0
    }
}
impl FormId for Counted {
    fn key(&self) -> RowKey {
        RowKey(self.0)
    }
}

#[test]
fn lookups_are_linear_in_the_row_count() {
    const N: u32 = 200;
    let mut sec = FormSection::<Counted, (), ()>::new("many");
    for i in 0..N {
        sec = sec.item(Counted(i), RowKind::Choice, (), Row::new(format!("r{i}")));
    }
    let mut t = FormTable::<Counted, (), ()>::new(1000);
    t.set(Form::new().section(sec), None);

    for target in [0u32, 100, N - 1] {
        CMP.with(|c| c.set(0));
        assert_eq!(t.index_of(&Counted(target)), Some(target as usize));
        let used = CMP.with(|c| c.get());
        assert!(used <= N as usize, "index_of took {used} comparisons for {N} rows");
        assert_eq!(used, target as usize + 1);
    }
    CMP.with(|c| c.set(0));
    assert_eq!(t.index_of(&Counted(9999)), None);
    assert!(CMP.with(|c| c.get()) <= N as usize);
    let _ = (Act::A, Act::B, Act::C, Act::D); // keep the sample enum fully used
}
