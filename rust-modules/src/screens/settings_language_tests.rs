//! Language persistence is install-wide; selection must not change this launch's locale.
use super::*;
use super::test_support::*;
use crate::i18n::Preference;
use crate::ui::form::FormId;
use crate::ui::machine::{Edge, InputEvent, InputKind, Source};
use crate::ui::present::Present;

fn activate(page: &mut LanguagePage, row: u32) -> Vec<Stamped<InnerHost>> {
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    page.activate(row, &mut fx);
    out
}

/// The focus element of a language's row — an identity, never its position.
fn lang(preference: Preference) -> u32 { LangId::Choice(preference).key().0 }
fn contribute() -> u32 { LangId::Contribute.key().0 }

struct SavedLanguage(Preference);
impl SavedLanguage {
    fn new(value: Preference) -> Self { Self(crate::i18n::saved_preference_for_test(value)) }
}
impl Drop for SavedLanguage {
    fn drop(&mut self) { crate::i18n::saved_preference_for_test(self.0); }
}

fn save_request(effects: Vec<Stamped<InnerHost>>) -> (Preference, std::sync::mpsc::Sender<bool>) {
    let mut requests = effects.into_iter().filter_map(|event| match event.fx {
        Fx::App(AppFx::Preferences(super::super::registry::PreferenceCmd::Language { language, reply })) => Some((language, reply)),
        _ => None,
    });
    let request = requests.next().expect("language save is an asynchronous application command");
    assert!(requests.next().is_none(), "one activation submits one save");
    request
}

#[test]
fn language_selection_waits_for_durable_receipt_and_keeps_running_locale() {
    let _guard = crate::testlock::serial();
    let _saved = SavedLanguage::new(Preference::System);
    let running = crate::i18n::current().language().tag();
    let mut page = LanguagePage::new(EntryId(0));
    let before = page.state.hash();
    let (requested, reply) = save_request(activate(&mut page, lang(Preference::Be)));
    assert_eq!(requested, Preference::Be);
    assert!(page.state.busy);
    assert_eq!(page.state.selected, Preference::System, "queue admission is not durable success");
    assert_ne!(page.state.hash(), before, "pending saves belong to replay state");
    assert!(!page.poll_save(), "waiting never blocks the frame thread");
    assert!(activate(&mut page, lang(Preference::Es)).is_empty(), "repeated saves are suppressed until the receipt");
    reply.send(true).unwrap();
    assert!(page.poll_save());
    assert_eq!(page.state.selected, Preference::Be);
    assert!(!page.state.busy && !page.state.failed);
    assert_eq!(crate::i18n::current().language().tag(), running);
    assert_eq!(page.pending(), Preference::Be != crate::i18n::current().preference());
}

#[test]
fn choosing_system_default_saves_the_preference_instead_of_resolved_language() {
    let _guard = crate::testlock::serial();
    let _saved = SavedLanguage::new(Preference::Be);
    let mut page = LanguagePage::new(EntryId(0));
    let (requested, reply) = save_request(activate(&mut page, lang(Preference::System)));
    assert_eq!(requested, Preference::System);
    assert_eq!(page.state.selected, Preference::Be);
    reply.send(true).unwrap();
    assert!(page.poll_save());
    assert_eq!(page.state.selected, Preference::System);
}

#[test]
fn language_entry_seats_the_engine_on_the_saved_preference() {
    let _guard = crate::testlock::serial();
    let _session = scratch_session("language-saved-seat");
    for preference in LANGUAGES.iter().copied() {
        let _saved = SavedLanguage::new(preference);
        let entry = EntryId(7);
        let mut surface = RouteSurface::new(
            entry, InstanceId(0), Family::Settings, SettingsPage::Language, crate::pms::HubsSnapshot::empty_for_test().view(),
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
        assert_eq!(engine.read(owner).current.map(|key| key.elem), Some(lang(preference)),
            "the first OK must address the saved language, {preference:?}");
    }
}

#[test]
fn failed_or_disconnected_language_save_keeps_confirmed_selection_and_can_retry() {
    let _guard = crate::testlock::serial();
    let _saved = SavedLanguage::new(Preference::En);
    for disconnected in [false, true] {
        let mut page = LanguagePage::new(EntryId(0));
        let (_, reply) = save_request(activate(&mut page, lang(Preference::Es)));
        if !disconnected { reply.send(false).unwrap(); }
        drop(reply);
        assert!(page.poll_save());
        assert_eq!(page.state.selected, Preference::En);
        assert!(page.state.failed && !page.state.busy);
        let (preference, reply) = save_request(activate(&mut page, lang(Preference::Es)));
        assert_eq!(preference, Preference::Es);
        assert!(page.state.busy && !page.state.failed);
        reply.send(true).unwrap();
        assert!(page.poll_save());
        assert_eq!(page.state.selected, Preference::Es);
    }
}

#[test]
fn contribution_is_focusable_and_right_opens_the_guide() {
    let _guard = crate::testlock::serial();
    let _session = scratch_session("language-contribution");
    let mut page = LanguagePage::new(EntryId(0));
    let key = FocusKey { entry: EntryId(0), elem: contribute() };
    let cx = cx(Some(key));
    assert!(<LanguagePage as Focusable<InnerHost>>::place(&page, &key.elem, &cx, At::SpringTarget).is_some());
    assert!(matches!(<LanguagePage as Focusable<InnerHost>>::neighbour(&page,
        FocusKey { entry: key.entry, elem: lang(Preference::Be) }, Dir::Down, &cx), Step::Move(next) if next == key));
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
    let mut surface = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root, crate::pms::HubsSnapshot::empty_for_test().view());
    step(&mut surface, ScreenEvent::Mount, None);
    step(&mut surface, ScreenEvent::Activate(root_key(RootId::Language)), None);
    assert_eq!(surface.inner.top().unwrap().arg, SettingsPage::Language);
    settle(&mut surface);
    let contribution = FocusKey { entry: EntryId(0), elem: contribute() };
    step(&mut surface, ScreenEvent::Activate(contribute()), Some(contribution));
    assert_eq!(surface.inner.top().unwrap().arg, SettingsPage::Contribute);
    settle(&mut surface);
    let effects = step(&mut surface, back_key(), None);
    assert_eq!(surface.inner.top().unwrap().arg, SettingsPage::Language);
    assert!(effects.iter().any(|effect| matches!(effect.fx,
        Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
            focus: FocusTarget::Elem(key),
        }))) if key == contribution)), "Back restores the contribution row, not the initial language seat");
}
