//! Consent-scope tests: notice/policy-version handling, first-run vs. settings commit paths,
//! and the rule that a scope's answer stands until the scope itself changes.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

// ---- should_show: the policy, byte for byte -------------------------------------------

/// **An automated boot is never asked.** `tests/run.py` injects a token and expects Home, the
/// fps scenes grade a heartbeat on a known route, and every `sim-shot` script drives a screen
/// it chose — a consent prompt in front of any of them would silently re-point the whole
/// harness at a screen nobody wrote an assertion for.
#[test]
fn an_automated_boot_never_sees_the_question() {
    assert!(!should_show(&Consent::default(), true));
    assert!(should_show(&Consent::default(), false), "…but an ordinary first boot does");
}

/// An answer against the current policy is not re-asked, whichever way it went.
#[test]
fn a_current_policy_answer_is_not_asked_again() {
    for (e, u) in [(false, false), (true, false), (false, true), (true, true)] {
        let answered = consent::apply(&Consent::default(), e, u, || Some("id".into()));
        assert!(!should_show(&answered, false), "re-asked after errors={e} usage={u}");
    }
}

/// A material schema expansion must receive two new explicit answers; the previous choice
/// stays fail-closed until the first-run route asks the expanded question again.
#[test]
fn a_policy_bump_reasks_without_reusing_the_old_answer() {
    let old = Consent {
        asked_version: consent::POLICY_VERSION - 1,
        errors: true,
        usage: true,
        install_id: Some("old-id".into()),
        errors_id: Some("old-errors-id".into()),
    };
    assert!(should_show(&old, false));
    let current = consent::apply(&Consent::default(), true, false, || Some("new-id".into()));
    assert!(!should_show(&current, false));
}

/// Each channel carries its own identifier, so each is refused on its own failed mint —
/// nothing here reads randomness itself; it only has to go through `consent::apply` with no
/// second path, which is what this pins.
#[test]
fn unavailable_randomness_refuses_the_channel_it_failed_for() {
    let answer = consent::apply(&Consent::default(), true, true, || None);
    assert!(answer.answered(), "the person is not asked again");
    assert!(!answer.errors && !answer.usage);
    assert!(answer.install_id.is_none() && answer.errors_id.is_none());
}

/// **The answer commits through the consent MACHINE, never through this screen.** The screen
/// emits `AppFx::Consent(Record)` and a `Pop`; it must never call
/// `telemetry::consent::install`/`apply` itself, which is exactly what would let a half-made
/// choice leak an event before the real machine has seen it.
#[test]
fn settings_done_commits_through_the_consent_machine_and_pops_the_surface() {
    let _g = crate::testlock::serial();
    let saved = consent::current();
    consent::install(Consent::default());
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    page.draft = (true, true);

    out.clear();
    page.band_commit(0, &mut mk_fx(&mut out, &mut present));
    assert!(
        out.iter().any(|s| matches!(
            &s.fx,
            Fx::App(AppFx::Consent(ConsentCmd::Record { errors: true, usage: true }))
        )),
        "the answer must go out as a command to the consent machine"
    );
    assert!(out.iter().any(|s| matches!(&s.fx, Fx::Nav(NavOp::Pop))), "and the surface must pop");
    assert_eq!(
        consent::current(),
        Some(Consent::default()),
        "the screen itself must not have installed anything — only the machine reading \
         `AppFx::Consent` may do that"
    );

    restore_consent_snapshot(saved);
}

/// LEFT/RIGHT is the answer row's whole navigation and decides nothing on its own — only OK
/// (`band_commit`) may write a decision. Declining must not carry the shared bit forward.
#[test]
fn declining_the_first_question_does_not_carry_the_shared_bit_forward() {
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
    out.clear();
    crash.band_commit(1, &mut mk_fx(&mut out, &mut present)); // "Don't share"
    assert!(
        out.iter().any(|s| matches!(
            &s.fx,
            Fx::Nav(NavOp::Push(SettingsPage::ConsentStage(bits))) if bits & ERRORS_SHARED == 0
        )),
        "declining crash reports must not answer the product question too"
    );
}

/// The second question's answer is combined with the FIRST one carried in its own page
/// argument and committed as one `Record` — never a second draft, and never two commands.
#[test]
fn the_product_stage_combines_both_answers_into_one_record_and_dismisses_the_surface() {
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    // Crash answered "Share" (the shared bit set), carried into the Product stage's own arg.
    let mut product = ConsentPage::first_run(
        EntryId(1),
        STAGE_PRODUCT | ERRORS_SHARED,
        &c,
        &mut mk_fx(&mut out, &mut present),
    );
    out.clear();
    product.band_commit(0, &mut mk_fx(&mut out, &mut present)); // "Share analytics"
    assert!(out.iter().any(|s| matches!(
        &s.fx,
        Fx::App(AppFx::Consent(ConsentCmd::Record { errors: true, usage: true }))
    )));
    assert!(
        out.iter().any(|s| matches!(&s.fx, Fx::Nav(NavOp::Dismiss(_)))),
        "the ceremony is answered: the whole surface leaves"
    );
}

// ---- real dispatch events, not only the direct calls above ----------------------------

/// A pointer click on a table row is `ScreenEvent::Activate`, dispatched by the engine
/// (`ui/dispatch.rs`'s pointer-resolve, `Activate::Direct`) — every OTHER test in this file
/// drives the row's effect through `row_commit` directly, which leaves the guard just added
/// to `Machine::step`'s `Activate` arm (and the `alert_index`/`band_index` routing beside it)
/// entirely ungraded by anything that looks like a real press.
#[test]
fn a_settings_toggle_commits_through_the_activate_event() {
    let _g = crate::testlock::serial();
    let saved = consent::current();
    consent::install(Consent::default());
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
    let errors_row = page.rows.iter().position(|r| *r == RowId::Errors).unwrap() as u32;

    out.clear();
    let handled = page.step(&ScreenEvent::Activate(errors_row), &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(handled, Handled::Yes);
    assert_eq!(page.draft, (true, false), "a real Activate — what a pointer click sends — must reach `row_commit`");

    restore_consent_snapshot(saved);
}

/// The band's twin of the test above: a real OK press or pointer release on a control is
/// `ScreenEvent::PressCommit`, routed by `cx.focus.current` — every other first-run test
/// drives `band_commit` directly. The alert's own mapping is pinned separately, above.
#[test]
fn a_first_run_answer_commits_through_the_presscommit_event() {
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let mut crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));

    out.clear();
    let mut cx = test_cx(&m);
    cx.focus.current = Some(FocusKey { entry: EntryId(1), elem: BAND }); // "Share reports"
    let handled = crash.step(&ScreenEvent::PressCommit(PressId(0)), &cx, &mut mk_fx(&mut out, &mut present));
    assert_eq!(handled, Handled::Yes);
    assert!(
        out.iter().any(|s| matches!(
            &s.fx,
            Fx::Nav(NavOp::Push(SettingsPage::ConsentStage(bits))) if bits & ERRORS_SHARED != 0
        )),
        "a real PressCommit — what an OK press or pointer release sends — must reach `band_commit`"
    );
}

// ---- content invariants: the two questions must not say the same thing ----------------

/// The two switches must be identified by different words, in both their title and their
/// sub-line — a copy-paste that made the crash and product rows share a label would still
/// compile, still lay out, and would say the same thing to a person trying to decide what to
/// share. Cheap, and worth having for exactly that reason: nothing else in this file would
/// notice.
#[test]
fn the_two_switches_name_two_different_purposes() {
    assert_ne!(ROW_ERRORS, ROW_USAGE);
    assert_ne!(ROW_ERRORS_SUB, ROW_USAGE_SUB);
}

/// The prose carries the four things WP260's first layer needs — who, why, that it is
/// optional, and where the rest is — plus the checkable payload claim, and the two questions
/// disclose DIFFERENT identifiers rather than one being a paraphrase of the other. Asserted
/// rather than eyeballed because a later edit for length is exactly how one of these goes
/// missing without anyone noticing on screen — the constants are long paragraphs, not a
/// caption a reviewer re-reads every time. (`privacy_policy()`'s own "names Sentry and
/// PostHog" claim moved with the function itself, to `screens::legal`, in phase 5b — that
/// half is that file's own invariant to keep now, not this one's.)
#[test]
fn first_run_separates_crash_and_product_consent() {
    assert!(CRASH_BODY.contains("signal"));
    assert!(CRASH_BODY.contains("product analytics identifier"));
    assert!(
        CRASH_BODY.contains("crash report identifier"),
        "the crash question must disclose the identifier it now carries"
    );
    assert!(PRODUCT_BODY.contains("random Analytics ID"));
    for body in [CRASH_BODY, PRODUCT_BODY] {
        assert!(
            body.contains("turn it off or sign out"),
            "each question must say the identifier ends with the sign-in, not with the television"
        );
    }
    assert!(PRODUCT_BODY.contains("exact viewing history"));
    assert_ne!(CRASH_TITLE, PRODUCT_TITLE);
}
