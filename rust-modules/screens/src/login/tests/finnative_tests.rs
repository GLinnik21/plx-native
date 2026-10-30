//! FinNative: the read-out's *Manual sign-in* and the form it opens.

use super::*;
use plx_jellyfin::jellyfin::backend::{take_sign_in_request, SignInMethod};

fn text(t: &str) -> ScreenEvent<SessionHost> {
    ScreenEvent::Input(InputEvent {
        at: plx_machine::machine::Tick::default(),
        source: Source::Script,
        kind: InputKind::Text(plx_machine::machine::TextEdit::Commit(t.into())),
    })
}

fn keyboard_up(effects: &[Stamped<SessionHost>]) -> Option<bool> {
    effects.iter().find_map(|st| match &st.fx {
        Fx::Deliver(_, Delivery::Keyboard { up }) => Some(*up),
        _ => None,
    })
}

fn starts_login(effects: &[Stamped<SessionHost>]) -> bool {
    effects.iter().any(|st| matches!(&st.fx, Fx::App(AppFx::Session(auth::SessionCmd::StartLogin))))
}

#[test]
fn the_qr_screen_and_a_failure_both_offer_manual_sign_in() {
    let _serial = plx_base::testlock::serial();
    let m = plx_ui::fixture::FixtureMeasure;
    let cx = test_cx(&m);
    let waiting = snapshot(Phase::Waiting, 1, "ABCD");
    let qr = LoginScreen::new(EntryId(0), waiting.read());
    assert_eq!(qr.row().as_slice(), [MANUAL], "a fresh code: Manual sign-in is the one control");
    assert!(Focusable::<SessionHost>::place(&qr, &MANUAL, &cx, At::Drawn).is_some());

    let failed = failed_with(auth::owner::IncidentState::NotNow);
    let s = LoginScreen::new(EntryId(0), failed.read());
    assert_eq!(s.row().as_slice(), [CONTROL, MANUAL], "Try again, then Manual sign-in");
    let (labels, _) = s.readout_labels();
    assert_eq!(labels[1], Some(plx_platform::i18n::msg::browse_jf_manual_c()));
}

/// FinNative has no reporting endpoint, so a failure the report could be offered for (every
/// failure is filed as one) raises no question over the read-out: *Try again* and *Manual
/// sign-in* are what it shows.
#[test]
fn a_failure_raises_no_report_question() {
    let _serial = plx_base::testlock::serial();
    let m = plx_ui::fixture::FixtureMeasure;
    let failed = failed_with(auth::owner::IncidentState::Offered {
        revision: plx_telemetry::telemetry::consent::revision(),
    });
    let mut s = LoginScreen::new(EntryId(0), failed.read());
    let (_, fx) = step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
    assert!(!enters_group(&fx, ALERT_GROUP), "no question is asked");
    assert!(!s.report.alert.is_open());
    assert_eq!(s.row().as_slice(), [CONTROL, MANUAL], "Try again, then Manual sign-in");
}

#[test]
fn manual_sign_in_opens_the_form_and_back_closes_it() {
    let _serial = plx_base::testlock::serial();
    let m = plx_ui::fixture::FixtureMeasure;
    let waiting = snapshot(Phase::Waiting, 1, "ABCD");
    let mut s = LoginScreen::new(EntryId(0), waiting.read());
    step_ev_with(&mut s, &ScreenEvent::Activate(MANUAL), &waiting, InstanceId(0), &m);
    assert!(s.form.is_some());
    assert!(s.state.form.is_some(), "the form is part of the logical state");
    let cx = test_cx(&m);
    let mut groups = Vec::new();
    Focusable::<SessionHost>::groups(&s, &cx, &mut groups);
    assert_eq!(groups.iter().map(|g| g.id).collect::<Vec<_>>(), [FORM_GROUP], "the form is the only group");
    let (handled, _) = step_ev_with(&mut s, &key_back_down(), &waiting, InstanceId(0), &m);
    assert_eq!(handled, Handled::Yes);
    assert!(s.form.is_none(), "BACK closes the form, not the screen");
}

#[test]
fn a_password_sign_in_is_typed_as_a_secret_and_starts_a_fresh_sign_in() {
    let _serial = plx_base::testlock::serial();
    let _ = take_sign_in_request();
    let m = plx_ui::fixture::FixtureMeasure;
    let waiting = snapshot(Phase::Waiting, 1, "ABCD");
    let mut s = LoginScreen::new(EntryId(0), waiting.read());
    step_ev_with(&mut s, &ScreenEvent::Activate(MANUAL), &waiting, InstanceId(0), &m);

    let (_, fx) = step_ev_with(&mut s, &ScreenEvent::Activate(form::SERVER), &waiting, InstanceId(0), &m);
    assert_eq!(keyboard_up(&fx), Some(true), "OK on a field brings the keyboard up");
    assert_eq!(Screen::<SessionHost>::text_entry(&s), plx_ui::screen::TextEntry::Plain);
    step_ev_with(&mut s, &text("nas.local:8096"), &waiting, InstanceId(0), &m);

    // Sign in with no username is refused in the form, and starts nothing.
    let (_, fx) = step_ev_with(&mut s, &ScreenEvent::Activate(form::SIGN_IN), &waiting, InstanceId(0), &m);
    assert!(!starts_login(&fx) && s.form.is_some());

    step_ev_with(&mut s, &ScreenEvent::Activate(form::USERNAME), &waiting, InstanceId(0), &m);
    step_ev_with(&mut s, &text("alice"), &waiting, InstanceId(0), &m);
    step_ev_with(&mut s, &ScreenEvent::Activate(form::PASSWORD), &waiting, InstanceId(0), &m);
    assert_eq!(Screen::<SessionHost>::text_entry(&s), plx_ui::screen::TextEntry::Secret,
        "a password is never recorded");
    step_ev_with(&mut s, &text("hunter2"), &waiting, InstanceId(0), &m);
    let mut probe = String::new();
    s.state.probe(&mut probe);
    assert!(!probe.contains("hunter2") && !probe.contains("alice"), "{probe}");

    let (_, fx) = step_ev_with(&mut s, &ScreenEvent::Activate(form::SIGN_IN), &waiting, InstanceId(0), &m);
    assert!(starts_login(&fx), "a fresh sign-in takes the request");
    assert_eq!(keyboard_up(&fx), Some(false), "the keyboard goes down with the form");
    assert!(s.form.is_none());
    let request = take_sign_in_request().expect("handed to the backend's one-shot slot");
    assert_eq!(request.server, "nas.local:8096");
    assert_eq!(request.method, SignInMethod::Password { username: "alice".into(), password: "hunter2".into() });
}
