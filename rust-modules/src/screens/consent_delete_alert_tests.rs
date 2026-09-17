//! Tests for the "Delete all local data" alert: opening it, confirming/cancelling it, and
//! its stray-input and dismissal-fade guards.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

// ---- the delete alert traps focus, and only the loop ever deletes ---------------------

/// Opening the alert traps focus on it — the same mechanism `first_run`'s own mount fix uses,
/// pointed at the alert's group instead of the band.
#[test]
fn the_delete_row_opens_the_alert_and_asks_to_be_reseated_on_it() {
    let _g = crate::testlock::serial();
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    let delete_row = page.rows.iter().position(|r| *r == RowId::Delete).unwrap() as i32;
    out.clear();
    page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));
    assert!(page.alert.is_open());
    assert!(requests_group(&out, ALERT_GROUP));
}

/// **Confirming asks the LOOP to sweep local data — this screen never touches disk itself.**
/// The legacy screen's `take_delete_request` seam moved one level up, to `AppFx::Loop`.
#[test]
fn confirming_the_alert_asks_the_loop_to_delete_and_reseats_the_table() {
    let _g = crate::testlock::serial();
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    page.alert.open_with_body(c"Delete all local data?", DELETE_SCOPE);
    page.alert.set_choice(AlertChoice::Destructive);
    out.clear();
    page.alert_answer(true, &mut mk_fx(&mut out, &mut present));
    assert!(!page.alert.is_open(), "input modality ends on the press frame, same as every other popover");
    assert!(out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Loop(LoopReq::DeleteAllLocalData)))));
    assert!(requests_group(&out, TABLE_GROUP));
}

/// Cancelling never asks the loop for anything destructive.
#[test]
fn cancelling_the_alert_never_asks_the_loop_to_delete_anything() {
    let _g = crate::testlock::serial();
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    page.alert.open_with_body(c"Delete all local data?", DELETE_SCOPE);
    page.alert.set_choice(AlertChoice::Cancel);
    out.clear();
    page.alert_answer(false, &mut mk_fx(&mut out, &mut present));
    assert!(!page.alert.is_open());
    assert!(!out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Loop(LoopReq::DeleteAllLocalData)))));
}

/// **The blocker this audit exists to fix.** A click that lands on the table or the band
/// underneath the "Delete all local data?" scrim resolves to the SAME `Activate`/`PressCommit`
/// events a legitimate press sends — the engine's hit map has no notion of a modal scrim, and
/// `ui/focus.rs`'s `set()` will already have written the clicked key as `cx.focus.current` by
/// the time either arrives — so the guard has to live in the events themselves. This is the
/// regression test for the two guards added to `Machine::step`'s `Activate` and `PressCommit`
/// arms: before them, this test's two assertions on `draft`/`out` would have failed.
#[test]
fn the_open_alert_refuses_a_stray_activate_or_presscommit_on_the_table_or_band() {
    let _g = crate::testlock::serial();
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    let delete_row = page.rows.iter().position(|r| *r == RowId::Delete).unwrap() as i32;
    page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));
    assert!(page.alert.is_open());
    let errors_row = page.rows.iter().position(|r| *r == RowId::Errors).unwrap() as u32;
    let draft_before = page.draft;

    // A click landing on the "Crash reports" row underneath the scrim: the same event a real
    // pointer press on that row sends (`ui/dispatch.rs`'s `Activate::Direct` -> `Activate`).
    out.clear();
    let handled = page.step(&ScreenEvent::Activate(errors_row), &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(handled, Handled::Yes, "the event is consumed rather than left to fall through anywhere else");
    assert_eq!(draft_before, page.draft, "…but it must not toggle the switch underneath the scrim");
    assert!(page.alert.is_open(), "…nor must it close the alert");

    // A click landing on the band's leading control underneath the scrim: the same event a
    // real OK press or pointer release on it sends (`Activate::Press` -> `PressCommit`).
    out.clear();
    let mut cx_band = test_cx(&m);
    cx_band.focus.current = Some(FocusKey { entry: EntryId(1), elem: BAND });
    page.step(&ScreenEvent::PressCommit(PressId(0)), &cx_band, &mut mk_fx(&mut out, &mut present));
    assert!(
        !out.iter().any(|s| matches!(&s.fx, Fx::Nav(NavOp::Pop))),
        "a stray PressCommit on a band element must not pop the surface while the alert is open"
    );
    assert!(page.alert.is_open(), "…nor must it close the alert");
}

/// **Pins the alert's index mapping through the REAL dispatch path** —
/// `ScreenEvent::PressCommit` routed by `cx.focus.current`, not the boolean shortcut
/// `alert_answer(bool)` the two tests above call directly. Without this, `FocusMoved`'s
/// mapping (`i == 1 => Destructive`), `ui/decision_alert.rs`'s two-element layout
/// (`frames() -> (cancel, destructive)`) and `alert_answer`'s own `i == 1` test could all
/// silently disagree about which element deletes, and every existing alert test would still
/// pass unchanged — a click on Cancel would erase the television and `make check` would stay
/// green.
#[test]
fn the_alert_index_mapping_is_pinned_through_a_real_press_commit() {
    let _g = crate::testlock::serial();
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    let delete_row = page.rows.iter().position(|r| *r == RowId::Delete).unwrap() as i32;

    // A freshly opened alert seats on Cancel (element 0) — `open_inner` resets `choice` to
    // `Choice::Cancel`, and `seat` is what the ENGINE actually calls on a fresh `Enter`, not a
    // property of `choice` this test could otherwise take on faith.
    out.clear();
    page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));
    assert!(page.alert.is_open());
    let placed = Placed {
        rect: Rect::new(0.0, 0.0, 0.0, 0.0),
        rest_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
        clip: Rect::FULL,
        index: None,
    };
    let seated = Focusable::<InnerHost>::seat(&page.view(), ALERT_GROUP, placed, &c);
    assert_eq!(seated.elem, ALERT, "a freshly opened alert seats on Cancel, element 0 — never the destructive answer");

    // Element 0 (Cancel) must never delete anything, driven through the real event `step`
    // dispatches — the shape `PressFrom::Pointer`/`PressFrom::Key` both funnel into.
    out.clear();
    let mut cx_cancel = test_cx(&m);
    cx_cancel.focus.current = Some(FocusKey { entry: EntryId(1), elem: ALERT });
    page.step(&ScreenEvent::PressCommit(PressId(0)), &cx_cancel, &mut mk_fx(&mut out, &mut present));
    assert!(
        !out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Loop(LoopReq::DeleteAllLocalData)))),
        "element 0 is Cancel and must never ask the loop to delete anything"
    );
    assert!(!page.alert.is_open(), "…but it does end the alert, same as every other answer");

    // Re-open fresh and pin the other end: element 1 (Destructive) is the ONLY one that may.
    out.clear();
    page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));
    let mut cx_delete = test_cx(&m);
    cx_delete.focus.current = Some(FocusKey { entry: EntryId(1), elem: ALERT + 1 });
    out.clear();
    page.step(&ScreenEvent::PressCommit(PressId(0)), &cx_delete, &mut mk_fx(&mut out, &mut present));
    assert!(
        out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Loop(LoopReq::DeleteAllLocalData)))),
        "element 1 is Destructive and must be the one that deletes"
    );
}

/// **The blocker this audit's verification pass found.** The dispatcher's own FIFO can
/// deliver a BACK key-down (which dismisses the alert via `alert_answer(false, ..)`) BEFORE
/// the `PressCommit` of an EARLIER OK-down against "Delete" that was already armed and
/// sitting in the queue: `ui/dispatch.rs::frame_with` builds a frame's `head` with key-down
/// events (step 2) ahead of the press machine's own `Commit`s (step 4), and a step's own
/// emissions — including `alert_answer`'s re-seat onto `TABLE_GROUP` — join the BACK of the
/// queue rather than running in place (`Dispatcher::absorb`). So the stale commit fires
/// with `cx.focus.current` still frozen at the destructive key, one frame before the re-seat
/// that would have moved it off ever runs. This test reproduces exactly that order by hand:
/// dismiss the alert first (as the earlier-queued BACK would), THEN deliver the `PressCommit`
/// with focus still pointing at the alert's destructive answer — the shape the real engine
/// hands this page for that one frame. Before the `is_open()` guard inside the `alert_index`
/// arm of `PressCommit`, this test's own assertion failed: the stale commit still asked the
/// loop to erase the television's local data, after the person had already cancelled.
#[test]
fn a_stale_presscommit_after_the_alert_was_already_dismissed_deletes_nothing() {
    let _g = crate::testlock::serial();
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    let delete_row = page.rows.iter().position(|r| *r == RowId::Delete).unwrap() as i32;
    page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));
    assert!(page.alert.is_open());

    // Focus, for real, sits on the destructive answer — the same key an already-armed OK
    // press committed against.
    let mut cx_delete = test_cx(&m);
    cx_delete.focus.current = Some(FocusKey { entry: EntryId(1), elem: ALERT + 1 });

    // BACK arrives FIRST in this frame's FIFO and dismisses the alert — `is_open()` flips
    // false at once, though the re-seat that would move focus off the stale key is only
    // queued, not yet applied.
    out.clear();
    let handled = page.step(&key_down(Key::Back), &cx_delete, &mut mk_fx(&mut out, &mut present));
    assert_eq!(handled, Handled::Yes);
    assert!(!page.alert.is_open(), "the BACK press must dismiss the alert");

    // The STALE `PressCommit` — from the OK-down against "Delete" armed before the BACK
    // arrived — is delivered next, still carrying the destructive focus key. It must not
    // resurrect the answer the person just cancelled.
    out.clear();
    page.step(&ScreenEvent::PressCommit(PressId(0)), &cx_delete, &mut mk_fx(&mut out, &mut present));
    assert!(
        !out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Loop(LoopReq::DeleteAllLocalData)))),
        "a PressCommit that outlives the alert's own dismissal must not delete anything"
    );
}

/// **The dismissal fade is trapped exactly like the open alert, not only up to the frame
/// `is_open()` flips false.** `Popover::dismiss` ends input modality on the SAME frame an
/// answer commits but leaves the panel and its scrim drawing at falling alpha for roughly
/// half a second more (`visible()`'s doc). This drives a real Cancel and then, with the
/// alert still `visible()` (fading, never ticked forward), the three concrete things a
/// stray press could have reached: a switch's `Activate`, the band's `PressCommit` (Done),
/// and a `Key::Right` at a row's trailing edge — which, aimed at the Delete row itself,
/// would silently RE-OPEN the very alert that is still dissolving on screen.
#[test]
fn the_dismissal_fade_traps_activate_presscommit_and_keys_the_same_as_the_open_alert() {
    let _g = crate::testlock::serial();
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    let delete_row = page.rows.iter().position(|r| *r == RowId::Delete).unwrap() as i32;
    page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));

    // Cancel: the alert dismisses (a fade begins) but stays `visible()` until that fade
    // lands — nothing here ever calls `alert.update`, so it never will during this test.
    out.clear();
    page.alert.set_choice(AlertChoice::Cancel);
    page.alert_answer(false, &mut mk_fx(&mut out, &mut present));
    assert!(!page.alert.is_open(), "Cancel closes the alert logically");
    assert!(page.alert.visible(), "…but it is still fading — the test is vacuous otherwise");

    // A click on a switch during the fade must not toggle it.
    let errors_row = page.rows.iter().position(|r| *r == RowId::Errors).unwrap() as u32;
    let draft_before = page.draft;
    out.clear();
    page.step(&ScreenEvent::Activate(errors_row), &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(draft_before, page.draft, "a click during the fade must not toggle the switch underneath it");

    // A stray band commit during the fade must not run Done, even with a real uncommitted
    // draft sitting behind it (fabricated here so there is a live Done to press at all).
    page.draft = (true, true);
    out.clear();
    let mut cx_band = test_cx(&m);
    cx_band.focus.current = Some(FocusKey { entry: EntryId(1), elem: BAND });
    page.step(&ScreenEvent::PressCommit(PressId(0)), &cx_band, &mut mk_fx(&mut out, &mut present));
    assert!(
        !out.iter().any(|s| matches!(&s.fx, Fx::Nav(NavOp::Pop))),
        "a stray band commit during the fade must not pop the surface (Done)"
    );

    // A RIGHT press at the Delete row's own trailing edge during the fade must not run
    // `row_commit` and re-open the very alert that is still fading out.
    out.clear();
    let mut cx_right = test_cx(&m);
    cx_right.focus.current = Some(FocusKey { entry: EntryId(1), elem: delete_row as u32 });
    page.step(
        &ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: Source::Script,
            kind: InputKind::Key { key: Key::Right, sym: 0, wcode: 0, edge: Edge::Down, at_edge: true },
        }),
        &cx_right,
        &mut mk_fx(&mut out, &mut present),
    );
    assert!(!page.alert.is_open(), "a RIGHT press during the fade must not re-open the alert");
}

/// **The reconcile fallback must preserve the alert's OWN choice, never reset it to Cancel.**
/// `FocusEngine::set` (`ui/focus.rs`) parks ANY resolved pointer hit into the engine's
/// persisted scope unconditionally — it has no notion that a modal is up, and a pointer
/// hovering the table underneath the alert's scrim still resolves against that table's own
/// hit stops (the `Activate` arm's own doc explains why those keep registering). The
/// dispatcher then asks this page's `reconcile` what to do with that stray key every frame,
/// and `ConsentView::reconcile` correctly refuses to let focus actually land outside the
/// alert while it is open — but before this fix it fell back to element 0 (Cancel)
/// UNCONDITIONALLY whenever the stray key was not itself an alert key, silently overwriting
/// whatever the person had chosen with the arrow keys. A keyboard user who had navigated
/// onto "Delete" and then only moved the mouse — no click, just a hover recomputing the
/// pointer's nearest stop — would find their selection reset to Cancel for no reason
/// visible on screen.
#[test]
fn reconcile_keeps_the_alerts_own_choice_when_a_stray_key_names_something_outside_it() {
    let _g = crate::testlock::serial();
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    let delete_row = page.rows.iter().position(|r| *r == RowId::Delete).unwrap() as i32;
    page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));
    assert!(page.alert.is_open());

    // The person arrow-key'd onto the destructive answer.
    page.alert.set_choice(AlertChoice::Destructive);

    // A stray pointer hover on some unrelated table row — exactly what `FocusEngine::set`
    // parks into the engine's persisted scope with no regard for the modal being up.
    let stray = FocusKey { entry: EntryId(1), elem: 0 };
    let reconciled = Focusable::<InnerHost>::reconcile(&page.view(), stray, &c);
    assert_eq!(
        reconciled,
        FocusKey { entry: EntryId(1), elem: ALERT + 1 },
        "reconcile must keep focus on the alert's OWN current choice (Destructive), not reset it to Cancel"
    );
}
