//! Settings > Services > Seerr through the real dispatcher: OK on the address row raises the
//! television's keyboard (the request leaves an INNER page of the Settings surface), what is typed
//! reaches the field, and OK lowers it again.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::frame;

#[test]
fn the_seerr_address_takes_the_keyboard_and_what_is_typed() {
    let _g = plx_base::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    open_settings_at(&mut d, plx_screens::family::SettingsPage::Seerr);
    for t in 1..40 {
        frame(&mut d, &mut rig, AppArg::Home, tick(t), vec![]);
    }
    assert!(!keyboard_takes_text(&d), "no keyboard up: typed text is nobody's");
    frame(&mut d, &mut rig, AppArg::Home, tick(40), script_key(Key::Ok, tick(40)));
    for t in 41..80 {
        frame(&mut d, &mut rig, AppArg::Home, tick(t), vec![]);
    }
    assert!(d.input.keyboard, "OK on the address row raises the television's keyboard");
    assert_eq!(rig.keyboard_calls, [true]);
    // The loop hands SDL text to the dispatcher only while this says so (`run.rs`): it used to be
    // Search alone, so text typed into a Settings page was queued for nobody.
    assert!(keyboard_takes_text(&d), "the page holding the keyboard takes what is typed");
    let text = |s: &str, at| InputEvent { at, source: Source::Script,
        kind: InputKind::Text(plx_machine::machine::TextEdit::Commit(s.into())) };
    frame(&mut d, &mut rig, AppArg::Home, tick(80), vec![text("h", tick(80))]);
    let probe = {
        let surface = d.nav.modals.surfaces.last().and_then(|s| s.entry.inst.as_ref()).expect("the Settings surface");
        let mut out = String::new();
        surface.screen.state().probe(&mut out);
        out
    };
    assert!(probe.contains("editing=true"), "{probe}");
    frame(&mut d, &mut rig, AppArg::Home, tick(81), script_key(Key::Ok, tick(81)));
    for t in 82..90 {
        frame(&mut d, &mut rig, AppArg::Home, tick(t), vec![]);
    }
    assert!(!d.input.keyboard, "OK keeps what was typed and lowers the keyboard");
    assert_eq!(rig.keyboard_calls, [true, false]);
}
