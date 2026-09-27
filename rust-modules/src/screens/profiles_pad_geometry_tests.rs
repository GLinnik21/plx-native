//! Pad grid geometry, footer/roster navigation, and focus-group membership.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn dispatcher_queued_pad_focus_reseat_does_not_reset_the_screen() {
    let mut s = bare(Pad::new());
    s.state.roster_n = 3;
    let mut present = Present::new();
    let mut effects = Vec::<Stamped<SessionHost>>::new();
    {
        let mut fx = Effects::new(
            &mut effects,
            MachineId::Instance(InstanceId(7)),
            &mut present,
        );
        s.open_pad(2, &mut fx);
    }
    assert!(s.state.pad_open);
    assert_eq!(s.state.pad_target, 2);
    let event = effects
        .into_iter()
        .find_map(|st| match st.fx {
            Fx::Deliver(owner, Delivery::Screen(event @ ScreenEvent::Enter(_))) => {
                assert_eq!(owner, MachineId::Instance(InstanceId(7)));
                Some(event)
            }
            _ => None,
        })
        .expect("opening must reseat the actual mounted instance");
    assert!(
        matches!(&event, ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(k) })
        if k.elem == pad_elem(0, 0))
    );
    step_ev(&mut s, &event, None);
    assert!(s.pad.open);
    assert_eq!(s.pad.target, 2);
    step_ev(&mut s, &key_down(Key::Other, b'7' as u32, 0), None);
    assert_eq!(s.pad.entry, "7");
    assert_eq!(s.state.pad_len, 1);
}

#[test]
fn declared_pad_holes_match_the_keys_and_edges() {
    let holes: Vec<(usize, usize)> = KEYS
        .iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, value)| value.is_none().then_some((i / PAD_COLS, i % PAD_COLS)))
        .collect();
    assert_eq!(PAD_HOLES, holes.as_slice());
    assert!(
        matches!(pad_neighbour(EntryId(1), pad_elem(3, 1), Dir::Right),
        Step::Move(key) if key.elem == pad_elem(3, 2))
    );
    assert!(matches!(
        pad_neighbour(EntryId(1), pad_elem(0, 0), Dir::Left),
        Step::Edge
    ));
}

#[test]
fn down_from_the_roster_reaches_the_footer_and_holds_there() {
    use crate::ui::focus::{FocusEngine, Outcome};
    let s = bare(Pad::new());
    let view = ProfilesView { screen: &s, n: 4 };
    let mut engine = FocusEngine::new();
    let owner = InputOwner::Entry(s.entry);
    engine.set(
        owner,
        FocusKey {
            entry: s.entry,
            elem: 2,
        },
        Some(ROSTER_GROUP),
        crate::ui::screen::By::Restore,
    );
    assert!(
        matches!(engine.move_dir(owner, &view, &[], Dir::Down, &cx(None)),
        Outcome::Moved { to, .. } if to.elem == FOOTER)
    );
    for dir in [Dir::Down, Dir::Left, Dir::Right] {
        let _ = engine.move_dir(owner, &view, &[], dir, &cx(None));
        assert_eq!(engine.current(owner).unwrap().elem, FOOTER);
    }
    assert!(
        matches!(engine.move_dir(owner, &view, &[], Dir::Up, &cx(None)),
        Outcome::Moved { to, .. } if to.elem < 4)
    );
}

#[test]
fn the_footer_is_reachable_with_no_roster_at_all() {
    let s = bare(Pad::new());
    let view = ProfilesView { screen: &s, n: 0 };
    let mut engine = crate::ui::focus::FocusEngine::new();
    let owner = InputOwner::Entry(s.entry);
    engine.enter(
        owner,
        &view,
        FocusTarget::ContainerGroup(ROSTER_GROUP),
        None,
        &cx(None),
    );
    assert_eq!(engine.current(owner).unwrap().elem, FOOTER);
}

#[test]
fn the_roster_neighbour_clamps_at_both_ends() {
    let s = bare(Pad::new());
    let view = ProfilesView { screen: &s, n: 4 };
    assert!(matches!(
        Focusable::<SessionHost>::neighbour(
            &view,
            FocusKey {
                entry: s.entry,
                elem: 0
            },
            Dir::Left,
            &cx(None)
        ),
        Step::Edge
    ));
    assert!(matches!(
        Focusable::<SessionHost>::neighbour(
            &view,
            FocusKey {
                entry: s.entry,
                elem: 3
            },
            Dir::Right,
            &cx(None)
        ),
        Step::Edge
    ));
}

/// The cell both walks below have to step over.
#[test]
fn the_keypad_has_exactly_one_hole_at_the_bottom_left() {
    assert_eq!(KEYS[3][0], None);
    assert!(KEYS.iter().flatten().filter(|c| c.is_none()).count() == 1);
}

/// The keypad's own walk, over every edge and hole case the shape actually has — ported from
/// `ui/profiles.rs`'s `the_keypad_walks_around_its_empty_cell`, over `pad_neighbour` rather
/// than the retired free functions it called directly.
#[test]
fn pad_neighbour_walks_around_its_empty_cell() {
    let e = EntryId(1);
    // DOWN from '7' (row 2, col 0): the hole sits directly below it, so the move DEFLECTS
    // sideways to the nearest occupied column in row 3 — '0', one column over — exactly as
    // the legacy `nearest_col` did. This is the case the engine's own fixture-only "skip
    // along the direction of travel" rule gets wrong for this grid: skipping straight down
    // from here runs off the bottom edge with nothing to land on (see the module doc).
    match pad_neighbour(e, pad_elem(2, 0), Dir::Down) {
        Step::Move(k) => assert_eq!(pad_rc(k.elem), Some((3, 1)), "▼ off 7 lands on 0"),
        Step::Edge => panic!("▼ off 7 must deflect onto 0, not dead-end at the hole"),
    }
    // DOWN from '8' (row 2, col 1) is an ordinary vertical move, straight to '0' — the
    // destination column is already occupied, so there is nothing to deflect.
    match pad_neighbour(e, pad_elem(2, 1), Dir::Down) {
        Step::Move(k) => assert_eq!(pad_rc(k.elem), Some((3, 1))),
        Step::Edge => panic!("row 2 col 1 has an occupied cell directly below it"),
    }
    // LEFT from '0' (row 3, col 1) runs into the hole and stops — LEFT/RIGHT skip a hole
    // WITHIN their own row rather than deflecting to a different one, and there is nothing
    // further left of it here.
    assert!(matches!(
        pad_neighbour(e, pad_elem(3, 1), Dir::Left),
        Step::Edge
    ));
    // RIGHT from delete (row 3, col 2) has nothing further right.
    assert!(matches!(
        pad_neighbour(e, pad_elem(3, 2), Dir::Right),
        Step::Edge
    ));
    // UP from '1' (row 0, col 0), the grid's own top-left corner, leaves entirely.
    assert!(matches!(
        pad_neighbour(e, pad_elem(0, 0), Dir::Up),
        Step::Edge
    ));
}

/// `pad_nearest_col` on its own, over exactly the three cases `ui/profiles.rs`'s retired
/// `nearest_col` was pinned against — the property `pad_neighbour`'s vertical arm now
/// restores after the engine's fixture-only "skip along the direction of travel" rule was
/// found to reach it only in DECLARATION (the `holes` field is inert in production; see the
/// module doc) while leaving DOWN off '7' a dead press in practice.
#[test]
fn pad_nearest_col_deflects_onto_the_nearest_occupied_column() {
    assert_eq!(pad_nearest_col(3, 0), 1, "▼ off 7 lands on 0");
    assert_eq!(
        pad_nearest_col(3, 2),
        2,
        "▼ off 9 lands on delete, which is under it"
    );
    assert_eq!(
        pad_nearest_col(1, 1),
        1,
        "an occupied column is kept as it is"
    );
}

// ---------------------------------------------------------------------------------------
// pure geometry — no font, no live auth
// ---------------------------------------------------------------------------------------

/// The pad's centred unit stays on the panel and the keypad sits BELOW the title with the
/// declared gap, whatever `Measure` answers — AND the unit is actually centred, not merely
/// on-panel. The three ordering checks alone do not pin that: a `pad_geom` that hard-coded
/// `unit_top` to some small on-panel constant (the exact pre-port bug this function exists to
/// fix — see its own doc, "it used to sit low on the panel") would still satisfy every
/// ordering assertion here while leaving a lopsided gap above the title and below the keypad.
#[test]
fn the_pad_is_one_centred_unit_with_the_keypad_below_the_title() {
    let m = FixtureMeasure;
    let (title_y, dots_y, grid_y) = pad_geom(&m);
    assert!(title_y > 0.0 && title_y < SCR_H as f32);
    assert!(grid_y > title_y, "the keypad sits below the title");
    assert!(
        dots_y > title_y && dots_y < grid_y,
        "the dot row sits between the two"
    );
    assert!(
        grid_y + PAD_GRID_H < SCR_H as f32,
        "the keypad fits on the panel"
    );
    // The unit's own top edge is `title_y` (the title is the first thing drawn in it) and its
    // bottom edge is `grid_y + PAD_GRID_H` (the keypad is the last) — a centred block puts an
    // EQUAL gap above and below those two edges.
    let top_gap = title_y;
    let bottom_gap = SCR_H as f32 - (grid_y + PAD_GRID_H);
    assert!(
        (top_gap - bottom_gap).abs() < 0.5,
        "the unit is not centred: {top_gap:.1}px above it, {bottom_gap:.1}px below it"
    );
}

/// `draw` and every `Focusable` query read `pad_key_rect` — pin the walk it is built from:
/// columns advance in x, rows in y, and the hole's own coordinates are never asked for a key
/// (there is no key THERE to draw or place).
#[test]
fn pad_key_rect_advances_by_column_and_row() {
    let m = FixtureMeasure;
    let r00 = pad_key_rect(&m, 0, 0);
    let r01 = pad_key_rect(&m, 0, 1);
    let r10 = pad_key_rect(&m, 1, 0);
    assert!((r01.x - r00.x - (PAD_KEY + PAD_KGAP)).abs() < 0.01);
    assert!((r00.x - r10.x).abs() < 0.01, "row does not move x");
    assert!((r10.y - r00.y - (PAD_KEY + PAD_KGAP)).abs() < 0.01);
    // `is_none()` rather than `assert_eq!(.., None)`: `Placed` deliberately carries no
    // `PartialEq` (it is geometry — two rects that differ in the last float are not a
    // meaningful inequality), and deriving one on a library type to satisfy one assertion
    // would put an equality into the engine's vocabulary that nothing else wants.
    assert!(
        pad_place(&m, pad_elem(3, 0)).is_none(),
        "no key exists at the hole"
    );
}

/// The footer pill widens with its label's own measured width — never a bare fixed box.
/// `r.w > 76.0` alone does not pin that: a `footer_rect` that returned a hard-coded `300.0`
/// (wider than the side padding by construction) would still pass it. Recomputing the exact
/// expected width from the SAME `Measure` call `footer_rect` makes internally is what actually
/// proves the pill tracks the label rather than a fixed number that happens to be bigger.
#[test]
fn the_footer_rect_widens_with_the_measured_label() {
    let m = FixtureMeasure;
    let tw = m.width(c"Sign out", theme::size::BODY, true);
    let r = footer_rect(&m);
    assert_eq!(
        r.w,
        tw + 76.0,
        "the pill's width must be the measured label plus its own side padding"
    );
    assert!(
        (r.cx() - SCR_W as f32 * 0.5).abs() < 0.01,
        "centred on the panel"
    );
}

// ---------------------------------------------------------------------------------------
// group topology — the pad traps focus; an empty roster still offers the footer
// ---------------------------------------------------------------------------------------

/// While the pad is open, `groups()` answers with ONE group and nothing else — the mechanism
/// that traps focus inside it (`ui/CLAUDE.md`'s "the pad is modal over the picker" doc): the
/// engine's geometric search has no other group to find in any direction.
#[test]
fn the_pad_is_the_only_group_while_it_is_open() {
    let s = bare(Pad::opened(0));
    let c = cx(None);
    let mut groups = Vec::new();
    Focusable::<SessionHost>::groups(&s, &c, &mut groups);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].id, PAD_GROUP);
    assert_eq!(groups[0].elem, ElemKind::Bare);
    assert_eq!(groups[0].len, PAD_ROWS * PAD_COLS);
    assert!(matches!(
        groups[0].kind,
        GroupKind::Grid { cols: PAD_COLS, .. }
    ));
}

/// With the pad closed and (as every host test's process necessarily has it, per
/// `ui/profiles.rs`'s own "an unseeded roster is the case under test") no live roster, the
/// footer is the ONLY group — "reachable even while the roster is empty/loading" — and it is
/// reachable with no source key at all (the engine's own "no focus yet" fallback,
/// `ui/focus.rs`'s `move_dir`).
#[test]
fn the_sign_out_pill_is_the_only_group_with_no_roster_on_screen() {
    let s = bare(Pad::new());
    let c = cx(None);
    let mut groups = Vec::new();
    Focusable::<SessionHost>::groups(&s, &c, &mut groups);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].id, FOOTER_GROUP);
    assert_eq!(groups[0].elem, ElemKind::Control);
    assert_eq!(
        Focusable::<SessionHost>::group_of(&s, &FOOTER, &c),
        Some(FOOTER_GROUP)
    );
    let from = Placed {
        rect: groups[0].extent,
        rest_rect: groups[0].extent,
        clip: Rect::FULL,
        index: None,
    };
    assert_eq!(
        Focusable::<SessionHost>::seat(&s, FOOTER_GROUP, from, &c).elem,
        FOOTER
    );
}

/// `reconcile` never strands focus off the footer when the roster is empty, and clamps a
/// stale roster index into range rather than losing it entirely otherwise.
#[test]
fn reconcile_falls_back_to_the_footer_with_an_empty_roster() {
    let s = bare(Pad::new());
    let c = cx(None);
    let stray = FocusKey {
        entry: EntryId(0),
        elem: 3,
    };
    assert_eq!(
        Focusable::<SessionHost>::reconcile(&s, stray, &c).elem,
        FOOTER
    );

    // A key already ON the footer must reconcile to ITSELF — a genuinely different branch
    // (`if want.elem == FOOTER { return want; }`) from the empty-roster fallback just above,
    // even though with an EMPTY roster both branches answer `elem == FOOTER` and so are
    // indistinguishable by that field alone: deleting the identity branch entirely would still
    // pass an assertion that only checks `.elem`, because the `n == 0` fallback also lands on
    // FOOTER. A FOREIGN `entry` id tells the two apart — the identity branch returns `want`
    // completely untouched, while the fallback builds a fresh key off `self.entry` and would
    // silently overwrite it.
    let foreign_entry = EntryId(99);
    let on_footer = FocusKey {
        entry: foreign_entry,
        elem: FOOTER,
    };
    let got = Focusable::<SessionHost>::reconcile(&s, on_footer, &c);
    assert_eq!(got.elem, FOOTER, "the footer reconciles to itself");
    assert_eq!(
        got.entry, foreign_entry,
        "…as an IDENTITY (the same key handed back), not a fresh one built off this screen's own entry"
    );
}

/// The half of the legacy `ui/profiles.rs`'s
/// `the_pill_answers_every_key_while_it_holds_focus` that had no analog on this screen, restored
/// when that module was deleted in phase 6.
///
/// Its ◀/▶/▲/▼ half is gone BY DESIGN and is not restored here: stepping the roster and
/// crossing to the footer are the shared focus engine's job now, and `ui/focus.rs`'s own suite
/// grades them. What was this screen's own, and only ever pinned there, is what OK MEANS.
#[test]
fn ok_means_one_thing_on_the_pill_and_another_on_a_tile() {
    const N: usize = 3;

    // the two live answers, and they are not interchangeable
    assert_eq!(commit_action(false, Some(FOOTER), N), Commit::SignOut);
    // the roster read-out's Back is the BACK key's request, never a sign-out
    assert_eq!(commit_action(false, Some(READOUT_BACK), 0), Commit::Back);
    assert_eq!(commit_action(false, Some(0), N), Commit::Select(0));
    assert_eq!(commit_action(false, Some(2), N), Commit::Select(2));

    // a tile index past the end of the roster commits NOTHING rather than clamping onto the
    // last one — the roster shrinks under this screen whenever a share is revoked
    assert_eq!(commit_action(false, Some(3), N), Commit::Nothing);
    assert_eq!(commit_action(false, Some(u32::MAX - 1), N), Commit::Nothing);

    // with no roster on screen the bottom arm has nothing to offer, but the pill still acts —
    // which is the whole reason the footer is reachable while the roster is still loading
    assert_eq!(commit_action(false, Some(0), 0), Commit::Nothing);
    assert_eq!(commit_action(false, Some(FOOTER), 0), Commit::SignOut);

    // nothing focused yet: a press cannot have attached to an element that does not exist
    assert_eq!(commit_action(false, None, N), Commit::Nothing);

    // and the pad swallows OK entirely. Its keys are Bare, so a commit arriving while it is
    // open was armed on the picker underneath — firing it would switch profile out from under
    // an open PIN prompt.
    assert_eq!(commit_action(true, Some(FOOTER), N), Commit::Nothing);
    assert_eq!(commit_action(true, Some(1), N), Commit::Nothing);
    assert_eq!(commit_action(true, None, N), Commit::Nothing);
}

// ---------------------------------------------------------------------------------------
// Reconciled from `main`'s own draft of this screen (commit 6745ca98), which staged an
// EARLIER `screens::profiles` as a test-only module and carried six tests this branch's
// newer file never had. The branch's file won the merge, so they are restored here rather
// than lost to it — adapted to this file's API (the pad's focused cell is the ENGINE's
// `cx.focus.current` now, not a `Pad::fr`/`fc` render mirror; the pad's horizontal walk is
// inlined in `pad_neighbour` rather than living in a free `pad_step_col`/`is_hole` pair), but
// with every assertion's intent and strength kept. Where a sibling test above already pins a
// SUPERSET of one of them, that is said in the ported test's own doc rather than used as a
// reason to drop it: a test only one deleted file ever carried is exactly what this
// repository's rules forbid losing to a refactor.
// ---------------------------------------------------------------------------------------

/// `PAD_HOLES` and `KEYS` are two independent declarations of the same fact — one drives the
/// engine's `GroupKind::Grid`, the other drives `draw_pad` and `pad_place` — so they are
/// pinned against each other CELL BY CELL, in both directions. The sibling
/// `declared_pad_holes_match_the_keys_and_edges` derives the hole list from `KEYS` and compares
/// the whole slice, which catches the same disagreement; this states it per cell, so a failure
/// names the offending `(r, c)` outright.
#[test]
fn pad_holes_matches_the_keys_table() {
    for r in 0..PAD_ROWS {
        for c in 0..PAD_COLS {
            assert_eq!(
                KEYS[r][c].is_none(),
                PAD_HOLES.contains(&(r, c)),
                "KEYS[{r}][{c}] and PAD_HOLES disagree about whether this cell is the hole"
            );
        }
    }
}

/// The keypad's bottom row has a blank where a phone dial pad has nothing: ◀ from `0` has no
/// key to its left, and ▼ off `7` lands on `0` rather than on the gap directly under it — the
/// legacy behaviour the module doc argues the generic grid-with-holes algorithm would get
/// wrong.
///
/// `main`'s draft asked its horizontal half of two free functions (`is_hole`, `pad_step_col`)
/// this file does not have: the hole test is `KEYS[r][c].is_none()` here and the column walk is
/// inlined in [`pad_neighbour`]'s `Left`/`Right` arm, so each of those assertions is made at
/// `pad_neighbour` instead. Same behaviour, one entry point lower — no production function was
/// added to satisfy a test.
#[test]
fn the_keypad_walks_around_its_empty_cell() {
    let e = EntryId(9);
    assert!(
        KEYS[3][0].is_none(),
        "the cell both walkers below have to step around"
    );

    // ◀ from '0' finds nothing to its left and holds; ▶ from '0' reaches delete.
    assert!(matches!(
        pad_neighbour(e, pad_elem(3, 1), Dir::Left),
        Step::Edge
    ));
    assert!(matches!(
        pad_neighbour(e, pad_elem(3, 1), Dir::Right),
        Step::Move(k) if k.elem == pad_elem(3, 2)
    ));
    // a row edge holds, on either side
    assert!(matches!(
        pad_neighbour(e, pad_elem(0, 0), Dir::Left),
        Step::Edge
    ));
    assert!(matches!(
        pad_neighbour(e, pad_elem(0, 2), Dir::Right),
        Step::Edge
    ));

    assert_eq!(pad_nearest_col(3, 0), 1, "▼ off '7' lands on '0'");
    assert_eq!(
        pad_nearest_col(3, 2),
        2,
        "▼ off '9' lands on delete, directly under it"
    );
    assert_eq!(
        pad_nearest_col(1, 1),
        1,
        "an occupied column is kept as it is"
    );

    match pad_neighbour(e, pad_elem(2, 0), Dir::Down) {
        Step::Move(k) => assert_eq!(
            k.elem,
            pad_elem(3, 1),
            "DOWN off '7' reaches '0', not the hole"
        ),
        Step::Edge => panic!("DOWN off '7' is a move"),
    }
    match pad_neighbour(e, pad_elem(0, 1), Dir::Up) {
        Step::Edge => {}
        Step::Move(_) => panic!("UP off the top row holds"),
    }
    match pad_neighbour(e, pad_elem(3, 1), Dir::Right) {
        Step::Move(k) => assert_eq!(k.elem, pad_elem(3, 2)),
        Step::Edge => panic!("RIGHT from '0' reaches delete"),
    }
}

/// **Focus is exclusive**: while the pad is open, `groups()` reports the grid ALONE — the
/// roster and the footer are not reachable by any direction, which is what "the pad traps
/// focus" means mechanically.
///
/// Driven through [`ProfilesView`] with a NON-EMPTY roster (`n: 3`), which is what this adds
/// over the sibling `the_pad_is_the_only_group_while_it_is_open`: here there are three retained
/// tiles and the pad still answers alone.
#[test]
fn only_the_grid_group_is_reachable_while_the_pad_is_open() {
    let s = bare(Pad::opened(0));
    let view = ProfilesView { screen: &s, n: 3 };
    let c = cx(None);
    let mut groups = Vec::new();
    Focusable::<SessionHost>::groups(&view, &c, &mut groups);
    assert_eq!(
        groups.len(),
        1,
        "no roster group and no footer group while the pad is up"
    );
    assert_eq!(groups[0].id, PAD_GROUP);
    assert_eq!(groups[0].len, PAD_ROWS * PAD_COLS);
}

