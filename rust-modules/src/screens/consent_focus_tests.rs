//! Focus and dispatch tests: band/table/list navigation, row commits, and the shared table
//! highlight spring, including the real-`FocusEngine` navigation checks in `mod composed`.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// **The regression `PreviewState::pos` exists to close.** Before that field, `PreviewState`
/// hashed only `which`, so a recorded `Privacy & data -> "Crashes / Errors" -> DOWN x5` flow
/// wrote five IDENTICAL state hashes: nothing else about this page changes as its document
/// scrolls — the route and every other surface word are untouched, and the ONE element here
/// (the reader) only scrolls, it never MOVES (`PreviewPage::step`'s own comment on the arm
/// this test drives, and `screens/legal.rs`'s `a_down_inside_a_document_changes_the_hashed_
/// state`, whose shape this test borrows for the sibling reader this file owns). A replay
/// against that recording could not have told a working `move_by` apart from one that
/// silently became a no-op, scrolled backwards, or moved by the wrong step — every one of
/// those builds would have replayed `verdict=SAME` across the whole document.
#[test]
fn a_down_inside_a_preview_changes_the_hashed_state() {
    let _guard = crate::testlock::serial();
    let idx = PreviewKind::ALL.iter().position(|k| *k == PreviewKind::Crash).unwrap() as u8;
    let mut page = PreviewPage::new(EntryId(9), idx);
    // Long enough that five DOWNs (5 * `document_reader::STEP` = 960px) never reach the end,
    // so every one of them is a genuine mid-document scroll rather than a clamp at `at_end()`.
    page.reader.set_extent_for_test(5000.0);
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();

    let mut hashes = vec![page.state.hash()];
    for _ in 0..5 {
        let handled = page.step(&key_down(Key::Down), &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(handled, Handled::Yes);
        hashes.push(page.state.hash());
    }

    // Every consecutive pair must differ — a stuck, reversed or mis-stepped `move_by` leaves
    // at least one pair identical, which is precisely the false `verdict=SAME` the unfixed
    // field produced across a whole five-DOWN sequence.
    for w in hashes.windows(2) {
        assert_ne!(
            w[0], w[1],
            "a DOWN inside the preview must change the hashed state, not just the render position"
        );
    }

    // Walking back UP must retrace the exact same sequence of hashes in reverse: the hash is
    // a function of the reading POSITION (`PreviewState::pos`, mirroring the reader's
    // settled `target`), never of how many keys have been pressed or which direction they
    // came from — a counter that only ever incremented would pass the loop above while
    // still being wrong.
    for expect in hashes.iter().rev().skip(1) {
        let handled = page.step(&key_down(Key::Up), &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(handled, Handled::Yes);
        assert_eq!(
            page.state.hash(),
            *expect,
            "walking back UP must retrace the same positions, not accumulate a new one"
        );
    }
}

// ---- the row set: count AND order, per mode and per first-run stage --------------------

/// The row list and the table it built cannot drift: a row added to one and not the other is
/// the index bug that makes a menu act on the wrong line. Count alone would pass on two rows
/// swapped, so order is asserted too, for every mode AND every first-run stage — the legacy
/// test's own point was that the Product stage had never been exercised at all, so the one
/// asymmetry `row_ids` can get wrong (which channel's preview a stage offers) went ungraded.
#[test]
fn every_row_id_has_a_row() {
    let _g = crate::testlock::serial();
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();

    let crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(crash.table.n_rows(), crash.rows.len() as i32, "first run, Crash stage");
    assert_eq!(crash.rows, vec![RowId::PreviewCrash, RowId::Policy], "Crash stage");

    out.clear();
    let product = ConsentPage::first_run(EntryId(1), STAGE_PRODUCT, &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(product.table.n_rows(), product.rows.len() as i32, "first run, Product stage");
    assert_eq!(product.rows, vec![RowId::PreviewUsage, RowId::Policy], "Product stage");

    out.clear();
    let settings = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(settings.table.n_rows(), settings.rows.len() as i32, "settings");
    assert_eq!(
        settings.rows,
        vec![
            RowId::Errors,
            RowId::Usage,
            RowId::PreviewCrash,
            RowId::PreviewUsage,
            RowId::Policy,
            RowId::ErrorsId,
            RowId::AnalyticsId,
            RowId::Delete,
        ],
        "settings"
    );
}

/// **Done is a route action after a change, never a table row** — a row appearing beside the
/// toggles would move every row index below it, and reversing the edit must remove it again
/// rather than leaving a stale action nobody can reach a live control for.
#[test]
fn toggling_a_switch_shows_done_without_resizing_the_table_and_reversing_hides_it_again() {
    let _g = crate::testlock::serial();
    let saved = consent::current();
    consent::install(Consent::default());
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    assert!(page.band_labels().is_empty(), "no Done until a value differs from what is stored");
    let rows_before = page.table.n_rows();

    out.clear();
    page.row_commit(0, &mut mk_fx(&mut out, &mut present)); // row 0 is Crash reports
    assert_eq!(page.draft, (true, false));
    assert_eq!(page.table.n_rows(), rows_before, "Done never changes table geometry");
    assert_eq!(page.band_labels().len(), 1, "exactly one action appears");

    out.clear();
    page.row_commit(0, &mut mk_fx(&mut out, &mut present));
    assert_eq!(page.draft, (false, false), "toggled back to the stored answer");
    assert!(page.band_labels().is_empty(), "…and Done goes away with it");

    restore_consent_snapshot(saved);
}

/// **First run must open on its answers, not on the reading list — the bug this audit exists
/// to catch.** `family.rs` fixes `GroupId(0)` as the TABLE group for every OTHER page in the
/// Settings family (the root, Legal, Favourite libraries, and this same screen's own Settings
/// mode all really do want to land on their list), and the container's generic mount always
/// asks for exactly `ContainerGroup(GroupId(0))` on a fresh entry (`ModalStack::present`).
/// Without an explicit correction the crash question would silently open with focus on "See
/// an example report" instead of "Share reports" — a press of OK there would open a document
/// instead of answering the question sitting in front of the person.
#[test]
fn first_run_requests_band_focus_on_its_own_mount() {
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let _crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
    assert!(
        requests_group(&out, BAND_GROUP),
        "the crash stage must ask to open on its answers, not the list beside them"
    );
}

/// **…and choosing an answer has to ask again, ordered after its own push.** The surface's
/// generic push (`RouteSurface::request`) mounts the Product stage — which asks for the band
/// on its own construction, exactly like the Crash stage above — and then re-seats focus on
/// ITS OWN default, `GroupId(0)`; because that re-seat is emitted AFTER the mount finishes, it
/// lands after (and so overrides) the mount's own request in the frame's effect queue. This
/// only proves this page's OWN half of the fix — that `band_commit` orders its re-ask after
/// its `Fx::Nav` — because the cross-file race with `RouteSurface::request`'s default lives in
/// `screens/settings.rs`, which this lane does not own; see the audit's report for the traced
/// argument that the ordering here is what makes the re-ask win that race.
#[test]
fn choosing_an_answer_re_asks_for_band_focus_after_its_own_push() {
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
    out.clear();
    crash.band_commit(0, &mut mk_fx(&mut out, &mut present)); // "Share reports"
    let nav_at = out
        .iter()
        .position(|s| matches!(s.fx, Fx::Nav(NavOp::Push(_))))
        .expect("a push to the product stage");
    let band_at = out
        .iter()
        .position(|s| requests_group(std::slice::from_ref(s), BAND_GROUP))
        .expect("a re-ask for the band");
    assert!(band_at > nav_at, "the re-ask must be ordered AFTER the push, or a competing default could win instead");
}

// ---- BACK: navigates; it never answers -------------------------------------------------

/// **BACK at the crash stage is the root of the ceremony.** There is nothing behind it to
/// restore — sign-in is the step behind it, and that cannot be undone — so it asks the LOOP
/// for the root press instead of discarding anything or doing nothing silently. Unlike the
/// legacy screen's `on_back` (a bare `bool` that could not tell "stepped back" and "swallowed"
/// apart, which `app/run.rs` records as the one mechanical reason the 2026-09-03 root rule was
/// never applied here), this page's `match self.mode` can, and does.
#[test]
fn back_at_the_crash_stage_asks_the_loop_for_the_root_press() {
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
    out.clear();
    let handled = crash.step(&key_down(Key::Back), &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(handled, Handled::Yes, "the crash stage answers BACK itself");
    assert!(
        out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Loop(LoopReq::BackAtRoot)))),
        "…by asking the loop for the root press, not by discarding anything"
    );
}

/// BACK at the Product stage is an ordinary step back, not a root press: this page declines
/// it so the surface's own generic "pop if depth > 1" can run the reverse of the push.
#[test]
fn back_at_the_product_stage_declines_so_the_surface_pops_it() {
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut product = ConsentPage::first_run(EntryId(1), STAGE_PRODUCT, &c, &mut mk_fx(&mut out, &mut present));
    out.clear();
    let handled = product.step(&key_down(Key::Back), &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(handled, Handled::No, "declined so the surface's own stack pop can run");
}

/// Settings BACK is likewise declined here — discarding the draft is simply what popping an
/// un-committed page does, and this page must not special-case it into its own dismissal.
#[test]
fn settings_back_is_declined_to_the_surfaces_own_pop() {
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    page.draft = (true, false);
    out.clear();
    let handled = page.step(&key_down(Key::Back), &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(handled, Handled::No);
    assert!(!out.iter().any(|s| matches!(&s.fx, Fx::Nav(_))), "this page pops nothing itself");
}

/// **The second half of this audit's LEFT-escape defect.** LEFT off the band's leading
/// control resolves, through `ui/dispatch.rs`'s OWN edge-rule redelivery, to exactly this
/// synthetic event — not a real BACK press (a real one always carries `at_edge: false`, see
/// `synthetic_left_edge_back`'s doc). Before the `at_edge` arm in `step`'s `Key::Back` match,
/// this fell into the SAME arm as a genuine press: at the crash stage it would have asked the
/// loop for the platform's root press — sending the television home on an arrow key — and at
/// the product stage it would have been declined, letting the surface's own generic pop
/// dismiss the WHOLE unanswered ceremony. Both stages must instead treat it as a pure wall.
#[test]
fn left_off_the_bands_leading_control_is_a_wall_at_every_first_run_stage() {
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();

    let mut crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
    out.clear();
    let handled = crash.step(&synthetic_left_edge_back(), &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(handled, Handled::Yes, "the crash stage swallows it");
    assert!(out.is_empty(), "…and does nothing at all — not even the root-press a real BACK asks for");

    let mut product = ConsentPage::first_run(EntryId(1), STAGE_PRODUCT, &c, &mut mk_fx(&mut out, &mut present));
    out.clear();
    let handled = product.step(&synthetic_left_edge_back(), &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(handled, Handled::Yes, "the product stage swallows it too");
    assert!(out.is_empty(), "…and must not walk the wizard back to Crash or dismiss the surface");
}

// ---- table motion: ported from legacy's own regression pins ---------------------------

/// **Ported from legacy's `toggling_a_value_preserves_in_flight_focus_motion`.** A value-only
/// `row_commit` calls `rebuild`, and `rebuild`'s `keep` expression (`let keep = sel >= 0 &&
/// self.table.n_rows() > 0;`) exists so the rebuild does not reset the shared `TableView`'s
/// highlight spring mid-flight — legacy's own test names the regression directly: "a
/// value-only rebuild snapped the pill". Nothing in the restructured file exercised `keep`
/// before this pin: every other row-commit test here only checks the resulting VALUE, never
/// the spring, so the one-line re-derivation could regress silently.
#[test]
fn a_value_only_row_commit_preserves_in_flight_focus_motion() {
    let _g = crate::testlock::serial();
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    // Row 1 (Usage) is a toggle: move the shared table's selection there and let its
    // highlight spring start travelling before the value-only rebuild that flipping it
    // triggers.
    page.table.move_sel(1);
    page.table.update(1.0 / 60.0, page.list_frame().h);
    let moving = page.table.highlight_motion();

    out.clear();
    page.row_commit(1, &mut mk_fx(&mut out, &mut present)); // flips Usage, calls `rebuild(1)`
    assert_eq!(
        page.table.highlight_motion(),
        moving,
        "a value-only rebuild must not teleport the highlight spring — `rebuild`'s `keep` flag exists exactly for this"
    );
}

/// **Ported from legacy's `the_consent_update_advances_the_shared_table_focus_pill`** — filed
/// there as "Regression for the TV report: the row selection changed its ink, but this screen
/// never advanced the TableView springs". The risk carries over unchanged: `ScreenEvent::Tick`'s
/// arm calls `self.table.update(dt, …)`, and nothing before this test drove a `Tick` through
/// `step` to prove that call is actually REACHED, rather than only present in the source.
#[test]
fn a_tick_advances_the_shared_table_highlight_spring() {
    let _g = crate::testlock::serial();
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    page.table.move_sel(1);
    let before = page.table.highlight_motion();

    page.step(&ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }), &c, &mut mk_fx(&mut out, &mut present));
    let after = page.table.highlight_motion();
    assert!(
        after.0 != before.0 || after.1.abs() > 0.0,
        "a Tick delivered through the machine must advance the shared table's highlight spring: {before:?} -> {after:?}"
    );
}

// ---- a mounting owns its OWN table: no channel left for one page to leak into another --

/// **Settings must open on its list whatever a first-run mounting left behind.** Legacy's
/// `TABLE` was a crate `static mut`, so a Settings page built after a first-run page in the
/// same process inherited whatever the static was left at — first run parks `list_focused =
/// false` (its focus is the answer band), so the inherited value silently drew Settings with
/// focus on nothing at all, while the keys still moved and committed an invisible selection.
/// **That channel does not exist any more to leak through**: `ConsentPage::bare` (this file,
/// above) builds a fresh `TableView::new()` into `self.table` on every call, so `first_run`
/// and `settings` each own a table that belongs to their OWN struct instance rather than to
/// the module. Proven here by holding both alive at once and mutating the first before the
/// second is ever built — the shape a shared static would have to survive, and a plain
/// owned-by-value struct field cannot: there is no code path left by which `settings`'s
/// table could even SEE what `crash`'s table did, `move` and borrow-checking having already
/// ruled it out at compile time, so the runtime assertions below are really about the
/// CONSTRUCTORS' own defaults rather than about aliasing that the type system already makes
/// impossible.
#[test]
fn settings_opens_on_the_list_after_a_first_run_left_focus_in_the_answer_band() {
    let _g = crate::testlock::serial();
    let saved = consent::current();
    consent::install(Consent::default());
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();

    let mut crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
    assert!(!crash.table.list_focused, "first run parks focus on the answers, not the list");
    // Stand in for whatever a live session would have left a SHARED static at, were there
    // still one to leave anything at — a selection made on a totally different question.
    crash.table.move_sel(1);

    out.clear();
    let settings = ConsentPage::settings(EntryId(2), &c, &mut mk_fx(&mut out, &mut present));
    assert!(
        settings.table.list_focused,
        "…and a wholly independent Settings mounting must put focus back on its OWN list"
    );
    assert_eq!(settings.table.sel, 0, "…starting at its own top row, not `crash`'s leftover selection");

    restore_consent_snapshot(saved);
}

// ---- rule 11 on the answer band: a hover parks, it never answers -----------------------

/// **Hovering never answers anything, on the one row this file still draws by hand.**
/// Reported against Share Crash Reports, as part of the same navigation complaint the
/// priority pins above answer: "hovering or arming answers nothing" was half of legacy's own
/// hover test, pinned separately here because its OTHER half — which pixel resolves to which
/// control, and that dead space resolves to none — is not this screen's mechanism any more.
/// `BandPart`'s hover/click policy (`Hover::Focus`, `Activate::Press`,
/// `ui/table_screen.rs`'s `BandPart::draw`) is the SAME generic stop every table row and
/// every OTHER family screen's band already goes through (`ui/hit.rs`'s `HitMap`), so a
/// regression in "does a hover park, does dead space park nothing" would break every band in
/// the app at once, not this one alone. **It cannot be re-proven through a real dispatch on
/// host at all**: the stops a pointer resolves against are only registered by a real `draw()`
/// pass (`f.stop(...)` inside `BandPart::draw`), and drawing measures text through SDL2_ttf
/// and issues GL calls that a host build has neither of — `screens::settings`'s own
/// composed-test module says the same of the whole family (`draw: false` on every frame it
/// runs, "not an optimization"). Settling THAT half needs `ui-sim` or a device. What stays
/// this screen's own to prove, and provable with no drawing at all, is that the EVENT a
/// hover produces — `ScreenEvent::FocusMoved` — never itself reaches `row_commit`/
/// `band_commit`/`alert_answer`: only `Activate` and `PressCommit` may.
///
/// **The name is deliberately narrower than the sentence at the top of this comment**, and
/// the re-green pass that audited these pins is why. An earlier spelling of it —
/// `hover_parks_an_answer_and_dead_space_parks_nothing` — promised the dead-space half that
/// the paragraph above spends nine lines explaining this body cannot check: there is no
/// `HitMap` here and no geometry, only a delivered `FocusMoved`. A test whose NAME claims
/// more than its assertions do is the precise failure this phase's audit was sent to find
/// (it found several), and it is worse than an owned gap, because the next reader greps for
/// "dead space", finds a green test, and stops looking. Legal's sibling
/// `a_hover_over_a_real_row_parks_it_and_a_click_opens_the_row_the_pointer_landed_on` is
/// where a real `HitMap` over real row geometry IS driven, dead space included; the half
/// that needs a real `draw()` is `ui-sim`'s or the television's, as above.
#[test]
fn a_hover_over_the_answer_band_parks_without_answering_anything() {
    let _g = crate::testlock::serial();
    let saved = consent::current();
    consent::install(Consent::default());
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    let draft_before = page.draft;

    out.clear();
    let handled = page.step(
        &ScreenEvent::FocusMoved {
            from: None,
            to: FocusKey { entry: EntryId(1), elem: BAND },
            by: crate::ui::screen::By::Pointer,
        },
        &c,
        &mut mk_fx(&mut out, &mut present),
    );
    assert_eq!(handled, Handled::Yes);
    assert_eq!(draft_before, page.draft, "a hover over the band must not answer anything");
    assert!(
        !out.iter().any(|s| matches!(&s.fx, Fx::Nav(_) | Fx::App(_))),
        "…and must not navigate or record anything either"
    );

    restore_consent_snapshot(saved);
}

/// **Toggling a switch must ask the engine for NOTHING.** Reported: "focus immediately jumps
/// to Done". Legacy's `on_ok` wrote `ACTION_FOCUSED = true` on every flip; the honest
/// re-proof now that focus lives entirely in the shared `FocusEngine` (never in a field this
/// screen owns) is that `row_commit`'s switch arms emit no `Fx::Deliver(.., Enter(..))` at
/// all — the ONLY channel this page has for moving focus anywhere (see `request_band_focus`
/// and the Delete row's own re-seat, above). With nothing asked for, the outer engine's
/// current key is left exactly where it was: on the row that was toggled. A regression that
/// reintroduces "ask for the band on every flip" would fail this immediately; a regression
/// that moved focus through some OTHER channel this screen does not have yet would not be
/// caught here — see the composed module below for what IS provable about the engine's own
/// answer to a direction key.
#[test]
fn toggling_a_switch_keeps_focus_on_the_row_it_toggled() {
    let _g = crate::testlock::serial();
    let saved = consent::current();
    consent::install(Consent::default());
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));

    out.clear();
    let handled = page.step(&ScreenEvent::Activate(0), &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(handled, Handled::Yes, "OK on the Crash reports switch");
    assert_eq!(page.draft, (true, false), "…flips exactly that switch");
    assert!(
        !out.iter().any(|s| matches!(&s.fx, Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(_))))),
        "a switch flip must ask the engine for NOTHING — not the band, not anywhere else; \
         whatever it asked for is where focus would jump to next, and the legacy bug asked \
         for Done explicitly"
    );

    restore_consent_snapshot(saved);
}

/// **No edit may leave focus on nothing.** Reported: "toggling the value back can cause
/// focus to disappear completely". Legacy parked focus on Done when a value first changed,
/// then removing Done (toggling back to the stored answer) left nothing to hold that focus —
/// the screen had no ring anywhere while the keys still moved an invisible selection. The
/// fix above (no re-seat request on EITHER toggle direction) makes the failure mode
/// structurally unreachable rather than merely patched: there is no re-seat request this
/// screen could have made that targets Done, so there is nothing for Done's disappearance to
/// strand. This test is the same toggle-and-reverse the report describes, watching for the
/// same absence on the way back down as the test above watches for on the way up.
#[test]
fn toggling_a_value_back_never_leaves_focus_on_nothing() {
    let _g = crate::testlock::serial();
    let saved = consent::current();
    consent::install(Consent::default());
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));

    page.row_commit(0, &mut mk_fx(&mut out, &mut present)); // Crash reports -> On, Done appears
    assert_eq!(page.draft, (true, false));
    assert_eq!(page.band_labels().len(), 1, "Done is now on screen");

    out.clear();
    page.row_commit(0, &mut mk_fx(&mut out, &mut present)); // …and back Off again
    assert_eq!(page.draft, (false, false), "the draft matches the stored answer again");
    assert!(page.band_labels().is_empty(), "…and Done goes away with it");
    assert!(
        !out.iter().any(|s| matches!(&s.fx, Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(_))))),
        "removing Done must not ask the engine to re-seat anywhere either — with nothing \
         requested on either edit, the engine's own current key never left the table row \
         being toggled, so there is no Done for it to have been stranded on"
    );

    restore_consent_snapshot(saved);
}

/// **Focus navigation, driven through the REAL shared engine — the level a navigation
/// regression pin now has to be re-proven at.** Every one of legacy's `on_left_right`/
/// `on_updown` bugs lived in a screen-local FSM that hand-rolled the walk between the answer
/// band and the reading list; that FSM is gone, and the walk is now `ui/focus.rs`'s
/// `FocusEngine`, reached through this page's `Focusable` view exactly as `ui/dispatch.rs`'s
/// `after_step` reaches it once a screen declines a directional key itself (§7.3 steps 2-5).
///
/// Driving the walk through a full `Dispatcher` + `RouteSurface` (the shape
/// `screens::settings`'s own `mod composed` stands up, since a Settings-family page never
/// owns a surface of its own) would need this file to name `screens::settings` — the one
/// thing `screens/mod.rs`'s layer rule ("a screen names `ui/`, `stores/`, the data crates and
/// this directory's `registry`; never … a sibling screen module") exists to forbid, and the
/// one file this audit does not own. So this module drives the engine directly against
/// `ConsentPage::view()` instead: the same query protocol (`groups`/`neighbour`/`place`/
/// `reconcile`/`seat`), the same edge rules (`ui/table_screen.rs`'s `TablePart`/`BandPart`),
/// and the same answer a real key press would get — one layer shorter than the composed
/// harness, but the REAL mechanism rather than a restatement of it.
mod composed {
    use super::*;
    use crate::ui::focus::{FocusEngine, Outcome};
    use crate::ui::screen::{By, Dir};

    /// One direction, through the real engine, against `page`'s live view — the composed
    /// twin of `page.step(...)` for the half of the focus protocol a screen's own `step`
    /// never answers.
    fn go(engine: &mut FocusEngine<u32>, owner: InputOwner, page: &ConsentPage, dir: Dir, c: &Cx<'_, InnerHost>) -> Outcome<u32> {
        engine.move_dir(owner, &page.view(), &[], dir, c)
    }

    /// **The second half of this audit's LEFT-escape defect, and the RIGHT half legacy never
    /// implemented at all.** Reported against Share Crash Reports: "focus cannot navigate
    /// correctly from the bottom buttons back to the options on the right." `on_left_right`
    /// answered LEFT/RIGHT only as a walk between the two answers and stopped dead at either
    /// end (rule 7 never reached the reading list from the trailing control). `BandPart`'s
    /// own RIGHT edge is `Geometric` unconditionally now — the same rule every family band
    /// uses — and `Seat::Remembered` on the band's own group is what makes the round trip
    /// back in land on the control that was actually left, not a hardcoded leading one.
    #[test]
    fn right_off_the_trailing_answer_reaches_the_reading_list() {
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let page = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
        let mut engine: FocusEngine<u32> = FocusEngine::new();
        let owner = InputOwner::Entry(EntryId(1));
        // First run's own mount asks the outer engine to open on the band's first control
        // (asserted separately by `first_run_requests_band_focus_on_its_own_mount`, above);
        // seed that premise here rather than re-deriving it, so this test is about the WALK.
        engine.set(owner, FocusKey { entry: EntryId(1), elem: BAND }, Some(BAND_GROUP), By::Dir);

        let outcome = go(&mut engine, owner, &page, Dir::Right, &c);
        assert!(matches!(outcome, Outcome::Moved { .. }), "RIGHT walks to the second answer: {outcome:?}");
        assert_eq!(engine.current(owner), Some(FocusKey { entry: EntryId(1), elem: BAND + 1 }));

        let outcome = go(&mut engine, owner, &page, Dir::Right, &c);
        let after = engine.current(owner).expect("focus is still seated somewhere");
        assert!(
            band_index(after.elem).is_none(),
            "RIGHT off the trailing answer must reach the list, not wrap or dead-end: {outcome:?}"
        );

        let outcome = go(&mut engine, owner, &page, Dir::Left, &c);
        assert!(matches!(outcome, Outcome::Moved { .. }), "LEFT walks back in: {outcome:?}");
        assert_eq!(
            engine.current(owner),
            Some(FocusKey { entry: EntryId(1), elem: BAND + 1 }),
            "…trailing control first, so the round trip is exact"
        );
    }

    /// **Reported: "from Done, Right does not return to the table, while Up strangely
    /// does."** `on_left_right` only ever answered for first run, so in Settings mode LEFT
    /// and RIGHT were both dropped on the floor and the band was a rightward dead end (rules
    /// 5 and 7). `TablePart`'s LEFT edge is `Geometric` whenever a band exists (rule 5) and
    /// `BandPart`'s RIGHT edge is `Geometric` unconditionally (rule 7) — both generic, both
    /// exercised here against this screen's real groups (a band only exists once a value
    /// differs from what is stored, so the test toggles one first, through `row_commit`
    /// exactly as a real OK on the switch would).
    #[test]
    fn left_reaches_the_action_band_and_right_comes_back_out_of_it() {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        consent::install(Consent::default());
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        page.row_commit(0, &mut mk_fx(&mut out, &mut present)); // Crash reports -> On
        assert_eq!(page.band_labels().len(), 1, "Done is on screen once a value differs");

        let mut engine: FocusEngine<u32> = FocusEngine::new();
        let owner = InputOwner::Entry(EntryId(1));
        engine.set(owner, FocusKey { entry: EntryId(1), elem: 0 }, Some(TABLE_GROUP), By::Dir);

        let outcome = go(&mut engine, owner, &page, Dir::Left, &c);
        assert!(matches!(outcome, Outcome::Moved { .. }), "LEFT from the list reaches the band: {outcome:?}");
        assert_eq!(
            engine.current(owner),
            Some(FocusKey { entry: EntryId(1), elem: BAND }),
            "…landing on Done, the band's only control"
        );

        let outcome = go(&mut engine, owner, &page, Dir::Right, &c);
        assert!(matches!(outcome, Outcome::Moved { .. }), "RIGHT from the band returns to the list: {outcome:?}");
        assert_eq!(
            engine.current(owner),
            Some(FocusKey { entry: EntryId(1), elem: 0 }),
            "…back on the row it left — `Seat::Remembered` on the table's own group, not just ANY row"
        );

        restore_consent_snapshot(saved);
    }

    /// UP leaves the band for the list and DOWN off the last row comes back — one vertical
    /// relationship, shared by both `TablePart`'s and `BandPart`'s own `Geometric` up/down
    /// edges, so the band is never a dead end from either side.
    #[test]
    fn the_answer_row_and_the_reading_list_are_one_vertical_walk() {
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let page = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
        let mut engine: FocusEngine<u32> = FocusEngine::new();
        let owner = InputOwner::Entry(EntryId(1));
        engine.set(owner, FocusKey { entry: EntryId(1), elem: BAND }, Some(BAND_GROUP), By::Dir);

        let outcome = go(&mut engine, owner, &page, Dir::Up, &c);
        assert!(matches!(outcome, Outcome::Moved { .. }), "UP leaves the band for the list: {outcome:?}");
        let after = engine.current(owner).expect("focus landed on the list");
        assert!(band_index(after.elem).is_none());

        // Seat on the list's LAST row before asking DOWN off it: the property under test is
        // that the LAST row (not the first) returns to the band.
        let last_row = page.table.n_rows() - 1;
        engine.set(owner, FocusKey { entry: EntryId(1), elem: last_row as u32 }, Some(TABLE_GROUP), By::Dir);
        let outcome = go(&mut engine, owner, &page, Dir::Down, &c);
        assert!(matches!(outcome, Outcome::Moved { .. }), "DOWN off the last row returns to the answers: {outcome:?}");
        assert_eq!(
            band_index(engine.current(owner).unwrap().elem),
            Some(0),
            "…landing on the band"
        );
    }
}
