//! Language persistence is install-wide; selection must not change this launch's locale.
use super::*;
use super::test_support::*;
use crate::i18n::Preference;
use crate::ui::machine::{Edge, InputEvent, InputKind, Source};
use crate::ui::present::Present;

fn activate(page: &mut LanguagePage, row: u32) -> Vec<Stamped<InnerHost>> {
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    page.activate(row, &mut fx);
    out
}

#[test]
fn choosing_language_preserves_credentials_and_applies_only_on_next_launch() {
    let _guard = crate::testlock::serial();
    let _session = multi_user_session("language-persistence");
    let before = crate::plex::session::peek();
    let running = crate::i18n::current().language().tag();
    let mut page = LanguagePage::new(EntryId(0));
    assert!(activate(&mut page, 3).is_empty());
    let saved = crate::plex::session::peek();
    assert_eq!(saved.language, Preference::Be);
    assert_eq!(saved.account_token, before.account_token);
    assert_eq!(saved.client_id, before.client_id);
    assert_eq!(saved.home_users.len(), before.home_users.len());
    assert_eq!(crate::i18n::current().language().tag(), running);
    let reopened = LanguagePage::new(EntryId(0));
    assert_eq!(reopened.state.selected, Preference::Be);
    assert_eq!(reopened.pending(), Preference::Be != crate::i18n::current().preference());
}

#[test]
fn choosing_system_default_persists_the_preference_instead_of_resolved_language() {
    let _guard = crate::testlock::serial();
    let _session = multi_user_session("language-system");
    let mut page = LanguagePage::new(EntryId(0));
    activate(&mut page, 2);
    activate(&mut page, 0);
    assert_eq!(crate::plex::session::peek().language, Preference::System);
    assert_eq!(page.state.selected, Preference::System);
}

#[test]
fn language_entry_seats_the_engine_on_the_saved_preference() {
    let _guard = crate::testlock::serial();
    let _session = scratch_session("language-saved-seat");
    for (index, preference) in LANGUAGES.iter().copied().enumerate() {
        crate::plex::session::save(&crate::plex::session::Session {
            language: preference,
            ..Default::default()
        });
        let entry = EntryId(7);
        let mut surface = RouteSurface::new(
            entry, InstanceId(0), Family::Settings, SettingsPage::Language,
        );
        let effects = step(&mut surface, ScreenEvent::Mount, None);
        let mut engine = crate::ui::focus::FocusEngine::new();
        let owner = crate::ui::machine::InputOwner::Entry(entry);
        // The modal lifecycle seats its generic group before draining queued mount effects.
        // Merely remembering another row after this does not move the current focus.
        engine.enter(owner, &surface, FocusTarget::ContainerGroup(GroupId(0)), None, &cx(None));
        for effect in effects {
            match effect.fx {
                Fx::Remember { group, elem } => engine.remember_projected(entry, group, elem),
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))) => {
                    engine.enter(owner, &surface, focus, None, &cx(None));
                }
                _ => {}
            }
        }
        assert_eq!(engine.read(owner).current.map(|key| key.elem), Some(index as u32),
            "the first OK must address the saved language, {preference:?}");
    }
}

#[test]
fn unavailable_session_does_not_show_an_unsaved_language_as_selected() {
    let _guard = crate::testlock::serial();
    let session = scratch_session("language-no-session");
    std::fs::remove_file(session.path()).unwrap();
    let mut page = LanguagePage::new(EntryId(0));
    activate(&mut page, 2);
    assert_eq!(page.state.selected, Preference::System);
    assert!(page.state.failed);
}

#[test]
fn contribution_is_focusable_and_right_opens_the_guide() {
    let _guard = crate::testlock::serial();
    let _session = scratch_session("language-contribution");
    let mut page = LanguagePage::new(EntryId(0));
    let key = FocusKey { entry: EntryId(0), elem: 4 };
    let cx = cx(Some(key));
    assert!(<LanguagePage as Focusable<InnerHost>>::place(&page, &key.elem, &cx, At::SpringTarget).is_some());
    assert!(matches!(<LanguagePage as Focusable<InnerHost>>::neighbour(&page,
        FocusKey { entry: key.entry, elem: 3 }, Dir::Down, &cx), Step::Move(next) if next == key));
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    page.step(&ScreenEvent::Input(InputEvent { at: Tick::default(), source: Source::Sdl,
        kind: InputKind::Key { key: Key::Right, sym: 0, wcode: 0, edge: Edge::Down, at_edge: true } }), &cx, &mut fx);
    assert!(out.iter().any(|effect| matches!(effect.fx, Fx::Nav(NavOp::Push(SettingsPage::Contribute)))));
    assert!(crate::ui::qr::QrCode::new(crate::i18n::CONTRIBUTE_URL).is_ok());
}

#[test]
fn signed_out_settings_reaches_language_and_back_restores_it_after_contribution() {
    let _guard = crate::testlock::serial();
    let _session = scratch_session("language-back");
    let mut surface = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
    step(&mut surface, ScreenEvent::Mount, None);
    step(&mut surface, ScreenEvent::Activate(3), None);
    assert_eq!(surface.inner.top().unwrap().arg, SettingsPage::Language);
    settle(&mut surface);
    let contribution = FocusKey { entry: EntryId(0), elem: 4 };
    step(&mut surface, ScreenEvent::Activate(4), Some(contribution));
    assert_eq!(surface.inner.top().unwrap().arg, SettingsPage::Contribute);
    settle(&mut surface);
    let effects = step(&mut surface, back_key(), None);
    assert_eq!(surface.inner.top().unwrap().arg, SettingsPage::Language);
    assert!(effects.iter().any(|effect| matches!(effect.fx,
        Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
            focus: FocusTarget::Elem(key),
        }))) if key == contribution)), "Back restores the contribution row, not the initial language seat");
}
