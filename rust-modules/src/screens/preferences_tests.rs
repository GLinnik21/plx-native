//! The field-list pages. The choice list's own transaction tests live beside it
//! (`preferences/picker_tests.rs`); this file also owns the fixtures both share.
use super::*;
use crate::plex::account::PreferenceRequest;
use crate::ui::machine::{FocusRead, InputOwner, PressRead, Stamped, Tick};
use crate::ui::present::Present;
use crate::ui::fixture::FixtureMeasure;

pub(super) fn context(focus: u32) -> Cx<'static, InnerHost> {
    static MEASURE: FixtureMeasure = FixtureMeasure;
    Cx { views: crate::stores::browse::DirectoryView::empty_for_test(), tick: Tick::default(),
        measure: &MEASURE, press: PressRead::default(),
        focus: FocusRead { current: Some(FocusKey { entry: EntryId(0), elem: focus }), ..Default::default() },
        owner: InputOwner::Entry(EntryId(0)) }
}
/// Step `page` with `ev` and return what it emitted.
pub(super) fn drive<M: Machine<InnerHost, Ev = ScreenEvent<InnerHost>>>(page: &mut M, ev: ScreenEvent<InnerHost>, focus: u32) -> Vec<Stamped<InnerHost>> {
    let mut out = Vec::new(); let mut present = Present::new();
    page.step(&ev, &context(focus), &mut Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present));
    out
}
pub(super) fn tick() -> ScreenEvent<InnerHost> { ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_000 }) }
pub(super) fn preference_commands(emitted: &[Stamped<InnerHost>]) -> usize {
    emitted.iter().filter(|e| matches!(&e.fx, Fx::App(AppFx::Preferences(_)))).count()
}
pub(super) fn popped(emitted: &[Stamped<InnerHost>]) -> bool {
    emitted.iter().any(|e| matches!(&e.fx, Fx::Nav(crate::ui::machine::NavOp::Pop)))
}
pub(super) fn pushed(emitted: &[Stamped<InnerHost>]) -> Vec<SettingsPage> {
    emitted.iter().filter_map(|e| match &e.fx { Fx::Nav(crate::ui::machine::NavOp::Push(p)) => Some(*p), _ => None }).collect()
}

/// A published account profile with a synthetic (request, snapshot) pair, restoring the previous
/// profile on drop. The caller holds `testlock::serial()`.
pub(super) struct Account {
    pub request: PreferenceRequest,
    pub snapshot: PreferenceSnapshot,
    user: crate::plex::session::UserRef,
    generation: u32,
    previous: std::sync::Arc<crate::plex::session::CurrentProfile>,
}
impl Account {
    pub fn new(uuid: &str, generation: u32, prefs: AudioPreferences) -> Self {
        let previous = crate::plex::session::current_snapshot();
        let user = crate::plex::session::UserRef { id: 7, uuid: uuid.into(), ..Default::default() };
        crate::plex::session::publish_profile_for_test(Some(user.clone()), generation);
        let (request, snapshot) = PreferenceRequest::fixture_for_test(user.clone(), generation, prefs);
        Self { request, snapshot, user, generation, previous }
    }
    /// A fresh receipt body for the same identity, carrying `prefs`.
    pub fn reply(&self, prefs: AudioPreferences) -> AccountPreferenceReply {
        let (request, snapshot) = PreferenceRequest::fixture_for_test(self.user.clone(), self.generation, prefs);
        AccountPreferenceReply { request: Some(request), outcome: Ok(snapshot) }
    }
    /// The profile moves on: every request captured before is now stale.
    pub fn go_stale(&self) {
        crate::plex::session::publish_profile_for_test(Some(self.user.clone()), self.generation + 1);
    }
}
impl Drop for Account {
    fn drop(&mut self) {
        crate::plex::session::publish_profile_for_test(self.previous.user.clone(), self.previous.generation);
    }
}
/// An Audio & Subtitles field list that has loaded `account`'s snapshot (and published it).
pub(super) fn loaded_audio_page(account: &Account) -> PreferencesPage {
    let mut page = PreferencesPage::new(EntryId(0), Kind::AudioSubtitles);
    page.txn.request = Some(account.request.clone());
    page.txn.snapshot = Some(account.snapshot.clone());
    page.txn.publish();
    page.rebuild(None);
    page
}

#[test]
fn account_constructor_is_inert_and_first_enter_emits_one_load_effect() {
    let _serial = crate::testlock::serial();
    let mut page = PreferencesPage::new(EntryId(0), Kind::AudioSubtitles);
    assert!(page.txn.pending.is_none());
    assert!(page.txn.request.is_none());
    assert!(page.state.io.status.is_empty());
    let mut emitted = drive(&mut page, ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::ContainerGroup(GroupId(0)) }), 0);
    emitted.extend(drive(&mut page, tick(), 0));
    assert!(page.state.io.busy);
    assert!(page.txn.request.is_none(), "the screen never captures live credentials");
    assert_eq!(emitted.iter().filter(|event| matches!(&event.fx,
        Fx::App(AppFx::Preferences(PreferenceCmd::Load { .. })))).count(), 1);
}

#[test]
fn both_language_pickers_offer_the_full_catalog_and_an_empty_preference() {
    for field in [PickerKind::AudioLanguage, PickerKind::SubtitleLanguage] {
        let options = field_options(field, Quality::Original, DirectPlayMode::Auto, None);
        assert_eq!(options.len(), crate::plex::languages::picker().count() + 1);
        assert_eq!(options[0].1, Value::Language(String::new()));
        assert!(options.len() > 100);
    }
}

/// Every field row is a `Nav` item onto its own picker page — the list pushes through the family's
/// one activation path and keeps no submenu of its own.
#[test]
fn every_field_row_pushes_its_picker_and_nothing_else() {
    let _serial = crate::testlock::serial();
    let _session = crate::plex::session::TempSession::new("pref-field-rows-push");
    let account = Account::new("pref-field-rows", 11, AudioPreferences::default());
    let mut playback = PreferencesPage::new(EntryId(0), Kind::Playback);
    let mut audio = loaded_audio_page(&account);
    for (page, fields) in [
        (&mut playback, vec![PickerKind::Quality, PickerKind::DirectPlay, PickerKind::SubtitleSize, PickerKind::SubtitlePosition, PickerKind::NextEpisode]),
        (&mut audio, vec![PickerKind::AudioLanguage, PickerKind::SubtitleMode, PickerKind::SubtitleLanguage, PickerKind::ForcedSubtitles]),
    ] {
        for field in fields {
            let key = RowId::Field(field).key().0;
            let emitted = drive(page, ScreenEvent::Activate(key), key);
            assert_eq!(pushed(&emitted), vec![SettingsPage::Picker(field)], "{field:?}");
            assert_eq!(preference_commands(&emitted), 0);
        }
    }
}

/// After a picker's durable write the list's own rows show the new value: the picker publishes the
/// confirmed snapshot and the list adopts it when it is returned to.
#[test]
fn the_field_list_shows_the_value_a_picker_committed_after_the_pop() {
    let _serial = crate::testlock::serial();
    let _session = crate::plex::session::TempSession::new("pref-parent-refresh");
    let account = Account::new("pref-parent-refresh", 12, AudioPreferences { subtitle_mode: 0, ..Default::default() });
    let mut parent = loaded_audio_page(&account);
    let mode_row = parent.form.index_of(&RowId::Field(PickerKind::SubtitleMode)).unwrap();
    assert_eq!(parent.state.values[mode_row], crate::i18n::msg::settings_audio_manual());
    let mut picker = PickerPage::new(EntryId(0), PickerKind::SubtitleMode);
    let emitted = drive(&mut picker, ScreenEvent::Activate(2), 2);
    let Some(Fx::App(AppFx::Preferences(PreferenceCmd::Save { reply, .. }))) = emitted.into_iter().map(|e| e.fx).find(|f| matches!(f, Fx::App(AppFx::Preferences(_)))) else {
        panic!("choosing Always must save");
    };
    reply.send(account.reply(AudioPreferences { subtitle_mode: 2, ..Default::default() })).unwrap();
    assert!(popped(&drive(&mut picker, tick(), 2)), "the durable receipt pops the picker");
    // The page beneath is re-entered by the pop.
    drive(&mut parent, ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::FirstInGroup(GroupId(0)) }), 0);
    assert_eq!(parent.state.values[mode_row], crate::i18n::msg::settings_audio_always(),
        "the list's readout is the committed value, with no reload");
}

/// The local fields have no snapshot to publish: the list re-reads the session values on its tick.
#[test]
fn the_field_list_rereads_a_local_value_a_picker_committed() {
    let _serial = crate::testlock::serial();
    let _session = crate::plex::session::TempSession::new("pref-parent-local");
    let previous = crate::route::quality();
    crate::route::restore_quality(Quality::Original);
    let mut parent = PreferencesPage::new(EntryId(0), Kind::Playback);
    let before = parent.state.values[0].clone();
    crate::route::restore_quality(Quality::P480);
    drive(&mut parent, tick(), 0);
    assert_ne!(parent.state.values[0], before);
    assert_eq!(parent.state.values[0], Quality::P480.label());
    crate::route::restore_quality(previous);
}

/// A Size/Position pick made anywhere (the player's Style pages publish the live value before
/// their write lands) shows in the list's cached read-outs on its next tick.
#[test]
fn the_field_list_rereads_a_subtitle_look_picked_elsewhere() {
    let _serial = crate::testlock::serial();
    let _session = crate::plex::session::TempSession::new("pref-parent-look");
    let (size, position) = (crate::route::subtitle_size(), crate::route::subtitle_position());
    crate::route::restore_subtitle_size(crate::route::SubtitleSize::Medium);
    crate::route::restore_subtitle_position(crate::route::SubtitlePosition::Low);
    let mut parent = PreferencesPage::new(EntryId(0), Kind::Playback);
    let size_row = parent.form.index_of(&RowId::Field(PickerKind::SubtitleSize)).unwrap();
    let position_row = parent.form.index_of(&RowId::Field(PickerKind::SubtitlePosition)).unwrap();
    assert_eq!(parent.state.values[size_row], crate::i18n::msg::settings_playback_subtitle_size_medium());

    crate::route::restore_subtitle_size(crate::route::SubtitleSize::Large);
    crate::route::restore_subtitle_position(crate::route::SubtitlePosition::High);
    drive(&mut parent, tick(), 0);
    assert_eq!(parent.state.values[size_row], crate::i18n::msg::settings_playback_subtitle_size_large());
    assert_eq!(parent.state.values[position_row], crate::i18n::msg::settings_playback_subtitle_position_high());
    crate::route::restore_subtitle_size(size);
    crate::route::restore_subtitle_position(position);
}
