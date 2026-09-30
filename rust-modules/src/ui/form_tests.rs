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

/// **A Nav item carries the drill-in chevron without a separate call**, and only a Nav item does;
/// an icon the caller chose explicitly is kept.
#[test]
fn a_nav_item_gets_the_chevron_from_its_kind() {
    use crate::ui::icons::Icon;
    let t = table(
        F::new().section(
            S::new("")
                .item(Id::A, nav(), Act::A, Row::new("Alpha"))
                .item(Id::B, RowKind::Toggle, Act::B, Row::new("Beta"))
                .item(Id::C, nav(), Act::C, Row::new("Gamma").ticon(Icon::Check)),
        ),
    );
    let icon = |i: usize| t.table.sections[0].rows[i].ticon;
    assert_eq!(icon(0), Some(Icon::Chevron));
    assert_eq!(icon(1), None);
    assert_eq!(icon(2), Some(Icon::Check));
    assert!(t.table.row_opens(0), "the table reads the chevron as 'this row opens a page'");
}

/// **A Choice row's checkmark comes from the current-value predicate** given at build.
#[test]
fn choice_rows_derive_checked_from_the_current_value() {
    let current = Id::B;
    let t = table(F::new().section(
        [(Id::A, Act::A), (Id::B, Act::B), (Id::C, Act::C)]
            .into_iter()
            .fold(S::new(""), |s, (id, act)| s.choice(id, act, Row::new("row"), |i| *i == current)),
    ));
    let checked: Vec<bool> = t.table.sections[0].rows.iter().map(|r| r.checked).collect();
    assert_eq!(checked, [false, true, false]);
    assert_eq!(t.binding_at(1).map(|b| b.kind.clone()), Some(RowKind::Choice));
}

/// **A disabled item is dim, still focusable, and never activates** (the `activate` is what OK and
/// RIGHT both go through).
#[test]
fn a_disabled_item_is_dim_focusable_and_inert() {
    let mut t = table(F::new().section(
        S::new("")
            .item(Id::A, RowKind::Button, Act::A, Row::new("Alpha"))
            .disabled(true)
            .item(Id::B, nav(), Act::B, Row::new("Beta"))
            .disabled(true)
            .item(Id::C, RowKind::Button, Act::C, Row::new("Gamma"))
            .disabled(false),
    ));
    assert!(t.table.sections[0].rows[0].dim && t.table.sections[0].rows[1].dim);
    assert!(!t.table.sections[0].rows[2].dim);
    assert_eq!(t.table.next_selectable(0, 1), Some(1), "focus can land on a disabled row");
    t.table.sel = 1;
    assert_eq!(t.selected_id(), Some(&Id::B));
    assert_eq!(t.activate(0), None);
    assert_eq!(t.activate(1), None, "a disabled Nav row does not push");
    assert_eq!(t.activate(2), Some(Activation::Action(Act::C)));
}

/// **`disabled` after a skipped `_if` disables nothing**: the "Style omitted during a server burn,
/// dim during the app's own burn" shape must not dim the row declared before the skipped one.
#[test]
fn disabled_after_a_skipped_item_does_not_reach_the_previous_row() {
    let t = table(F::new().section(
        S::new("")
            .item(Id::A, RowKind::Button, Act::A, Row::new("Alpha"))
            .item_if(false, Id::B, RowKind::Button, Act::B, Row::new("Beta"))
            .disabled(true)
            .item_keyed_if(false, Id::C, RowKey(9), RowKind::Button, Act::C, Row::new("Gamma"))
            .disabled(true),
    ));
    assert!(!t.table.sections[0].rows[0].dim, "Alpha is not the skipped row");
    assert_eq!(t.activate(0), Some(Activation::Action(Act::A)));
    // and a later real item is still disableable
    let t = table(F::new().section(
        S::new("")
            .item_if(false, Id::A, RowKind::Button, Act::A, Row::new("Alpha"))
            .item(Id::B, RowKind::Button, Act::B, Row::new("Beta"))
            .disabled(true),
    ));
    assert!(t.table.sections[0].rows[0].dim);
}

fn tall_page(ids: &[Id]) -> F {
    F::new().section(ids.iter().cloned().fold(S::new(""), |s, id| {
        s.item(id, RowKind::Button, Act::A, Row::new("row"))
    }))
}

/// **`open` focuses the given id with the scroll at the top**, falling back to the opening row.
#[test]
fn open_snaps_to_the_initial_id_at_scroll_zero() {
    let mut t = table(sample(true));
    t.restore(sample(true), Some(&Id::A), 90.0);
    t.open(sample(true), Some(&Id::C));
    assert_eq!(t.selected_id(), Some(&Id::C));
    assert_eq!(t.table.scroll_pos(), 0.0);
    t.open(sample(true), Some(&Id::Dup)); // not on the page
    assert_eq!(t.table.sel, t.table.opening_row(), "a missing initial id falls back to the opening row");
    t.open(sample(true), None);
    assert_eq!(t.table.sel, 0);
}

/// **`refresh` keeps the scroll and the selected id** when the page's data changes under the viewer,
/// and a vanished selection falls to its neighbour.
#[test]
fn refresh_keeps_scroll_and_restores_selection_by_id() {
    let mut t = table(tall_page(&[Id::B, Id::C, Id::D]));
    t.restore(tall_page(&[Id::B, Id::C, Id::D]), Some(&Id::C), 60.0);
    // a row appears above the selection
    t.refresh(tall_page(&[Id::A, Id::B, Id::C, Id::D]));
    assert_eq!(t.selected_id(), Some(&Id::C), "selection follows its id");
    assert_eq!(t.table.sel, 2);
    assert_eq!(t.table.scroll_pos(), 60.0, "refresh does not jump the scroll");
    // the selected row vanishes: the next survivor takes it
    t.refresh(tall_page(&[Id::A, Id::B, Id::D]));
    assert_eq!(t.selected_id(), Some(&Id::D));
    assert_eq!(t.table.scroll_pos(), 60.0);
}

/// **`restore` reinstates a saved selection and scroll** (the pop back to a scrolled page).
#[test]
fn restore_reinstates_the_saved_selection_and_scroll() {
    let mut t = table(sample(true));
    t.restore(sample(true), Some(&Id::D), 75.0);
    assert_eq!(t.selected_id(), Some(&Id::D));
    assert_eq!(t.table.scroll_pos(), 75.0);
    t.restore(sample(true), Some(&Id::Dup), 10.0);
    assert_eq!(t.table.sel, t.table.opening_row(), "a saved id that is gone falls back to the opening row");
    assert_eq!(t.table.scroll_pos(), 10.0);
}

/// Change profile / Sign out (destructive) / Settings: the destructive row is the vanished row's
/// NEXT neighbour, and the safe row comes after it.
fn menu(change: bool) -> F {
    let mut out = Row::new("Sign out");
    out.destructive = true;
    F::new().section(
        S::new("")
            .item_if(change, Id::A, RowKind::Button, Act::A, Row::new("Change profile"))
            .item(Id::C, RowKind::Button, Act::C, out)
            .item(Id::B, RowKind::Button, Act::B, Row::new("Settings")),
    )
}

#[test]
fn a_menu_rebuild_never_slides_onto_a_destructive_neighbour() {
    let mut t = table(menu(true));
    t.set(menu(false), Some(&Id::A));
    assert_eq!(t.selected_id(), Some(&Id::C), "rig: the neighbour rule lands on the destructive row");

    let mut t = table(menu(true));
    t.set_or_open(menu(false), Some(&Id::A));
    assert_eq!(t.selected_id(), Some(&Id::B), "a vanished keep opens on the opening row instead");

    let mut t = table(menu(true));
    t.set_or_open(menu(true), Some(&Id::C));
    assert_eq!(t.selected_id(), Some(&Id::C), "a surviving keep is kept, destructive or not");
}

#[test]
fn set_sliding_keeps_the_pill_gliding_where_set_snaps() {
    let mut t = table(sample(true));
    t.table.sel = 5;
    t.table.move_sel(-1);
    t.table.update(1.0 / 60.0, 600.0);
    let moving = t.table.highlight_motion();
    t.set_sliding(sample(true), Some(&Id::D));
    assert_eq!(t.table.highlight_motion(), moving, "a sliding rebuild leaves the pill in flight");

    let mut t = table(sample(true));
    t.table.sel = 5;
    t.table.move_sel(-1);
    t.table.update(1.0 / 60.0, 600.0);
    let moving = t.table.highlight_motion();
    t.set(sample(true), Some(&Id::D));
    assert_ne!(t.table.highlight_motion(), moving, "set snaps the pill to its landing");
}

#[test]
fn key_helpers_resolve_by_identity_and_step_over_inert_rows() {
    let t = table(sample(true));
    assert_eq!(t.focusable_len(), 4, "A, B, C, D — the separator and the note are not rows");
    assert_eq!(t.selected_key(), Some(Id::A.key()));
    assert_eq!(t.opening_key(), Some(Id::A.key()));
    assert_eq!(t.step_key(Id::B.key(), 1), Some(Id::C.key()), "the separator is stepped over");
    assert_eq!(t.step_key(Id::C.key(), 1), Some(Id::D.key()), "…and so is the note");
    assert_eq!(t.step_key(Id::D.key(), 1), None, "a menu never wraps");
    assert_eq!(t.step_key(Id::A.key(), -1), None);
    assert_eq!(t.step_key(RowKey(999), 1), None, "an unknown key has no neighbour");
}
