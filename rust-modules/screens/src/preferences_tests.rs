//! The field-list pages. The choice list's own transaction tests live beside it
//! (`preferences/picker_tests.rs`); this file also owns the fixtures both share.
use super::*;
use plx_plex::plex::account::PreferenceRequest;
use plx_machine::machine::{FocusRead, InputOwner, PressRead, Stamped, Tick};
use plx_machine::present::Present;
use plx_ui::fixture::FixtureMeasure;

pub fn context(focus: u32) -> Cx<'static, InnerHost> {
    static MEASURE: FixtureMeasure = FixtureMeasure;
    Cx { views: plx_data::stores::browse::DirectoryView::empty_for_test(), tick: Tick::default(),
        measure: &MEASURE, press: PressRead::default(),
        focus: FocusRead { current: Some(FocusKey { entry: EntryId(0), elem: focus }), ..Default::default() },
        owner: InputOwner::Entry(EntryId(0)) }
}
/// Step `page` with `ev` and return what it emitted.
pub fn drive<M: Machine<InnerHost, Ev = ScreenEvent<InnerHost>>>(page: &mut M, ev: ScreenEvent<InnerHost>, focus: u32) -> Vec<Stamped<InnerHost>> {
    let mut out = Vec::new(); let mut present = Present::new();
    page.step(&ev, &context(focus), &mut Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present));
    out
}
pub fn tick() -> ScreenEvent<InnerHost> { ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_000 }) }
pub fn preference_commands(emitted: &[Stamped<InnerHost>]) -> usize {
    emitted.iter().filter(|e| matches!(&e.fx, Fx::App(AppFx::Preferences(_)))).count()
}
pub fn popped(emitted: &[Stamped<InnerHost>]) -> bool {
    emitted.iter().any(|e| matches!(&e.fx, Fx::Nav(plx_machine::machine::NavOp::Pop)))
}
pub fn pushed(emitted: &[Stamped<InnerHost>]) -> Vec<SettingsPage> {
    emitted.iter().filter_map(|e| match &e.fx { Fx::Nav(plx_machine::machine::NavOp::Push(p)) => Some(*p), _ => None }).collect()
}

/// A published account profile with a synthetic (request, snapshot) pair, restoring the previous
/// profile on drop. The caller holds `testlock::serial()`.
pub struct Account {
    pub request: PreferenceRequest,
    pub snapshot: PreferenceSnapshot,
    user: plx_plex::plex::session::UserRef,
    generation: u32,
    previous: std::sync::Arc<plx_plex::plex::session::CurrentProfile>,
}
impl Account {
    pub fn new(uuid: &str, generation: u32, prefs: AudioPreferences) -> Self {
        let previous = plx_plex::plex::session::current_snapshot();
        let user = plx_plex::plex::session::UserRef { id: 7, uuid: uuid.into(), ..Default::default() };
        plx_plex::plex::session::publish_profile_for_test(Some(user.clone()), generation);
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
        plx_plex::plex::session::publish_profile_for_test(Some(self.user.clone()), self.generation + 1);
    }
}
impl Drop for Account {
    fn drop(&mut self) {
        plx_plex::plex::session::publish_profile_for_test(self.previous.user.clone(), self.previous.generation);
    }
}
/// An Audio & Subtitles field list that has loaded `account`'s snapshot (and published it).
pub fn loaded_audio_page(account: &Account) -> PreferencesPage {
    let mut page = PreferencesPage::new(EntryId(0), Kind::AudioSubtitles);
    page.txn.request = Some(account.request.clone());
    page.txn.snapshot = Some(account.snapshot.clone());
    page.txn.publish();
    page.rebuild(None);
    page
}

#[test]
fn account_constructor_is_inert_and_first_enter_emits_one_load_effect() {
    let _serial = plx_base::testlock::serial();
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
        assert_eq!(options.len(), plx_plex::plex::languages::picker().count() + 1);
        assert_eq!(options[0].1, Value::Language(String::new()));
        assert!(options.len() > 100);
    }
}

/// Every field row is a `Nav` item onto its own picker page — the list pushes through the family's
/// one activation path and keeps no submenu of its own.
#[test]
fn every_field_row_pushes_its_picker_and_nothing_else() {
    let _serial = plx_base::testlock::serial();
    let session = plx_plex::plex::session::TempSession::new("pref-field-rows-push");
    let account = Account::new("pref-field-rows", 11, AudioPreferences::default());
    // The Playback rows open only for a seated profile (an unseated page lists them read-only).
    seat_named(&session, "pref-field-rows", "Maya");
    let mut playback = PreferencesPage::new(EntryId(0), Kind::Playback);
    let mut audio = loaded_audio_page(&account);
    for (page, fields) in [
        (&mut playback, vec![PickerKind::Quality, PickerKind::DirectPlay, PickerKind::SubtitleSize, PickerKind::SubtitlePosition, PickerKind::NextEpisode, PickerKind::DeckPress, PickerKind::SkipInterval, PickerKind::InstantDpadSeeking, PickerKind::DolbyVision]),
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
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("pref-parent-refresh");
    let account = Account::new("pref-parent-refresh", 12, AudioPreferences { subtitle_mode: 0, ..Default::default() });
    let mut parent = loaded_audio_page(&account);
    let mode_row = parent.form.index_of(&RowId::Field(PickerKind::SubtitleMode)).unwrap();
    assert_eq!(parent.state.values[mode_row], plx_platform::i18n::msg::settings_audio_manual());
    let mut picker = PickerPage::new(EntryId(0), PickerKind::SubtitleMode);
    let emitted = drive(&mut picker, ScreenEvent::Activate(2), 2);
    let Some(Fx::App(AppFx::Preferences(PreferenceCmd::Save { reply, .. }))) = emitted.into_iter().map(|e| e.fx).find(|f| matches!(f, Fx::App(AppFx::Preferences(_)))) else {
        panic!("choosing Always must save");
    };
    reply.send(account.reply(AudioPreferences { subtitle_mode: 2, ..Default::default() })).unwrap();
    assert!(popped(&drive(&mut picker, tick(), 2)), "the durable receipt pops the picker");
    // The page beneath is re-entered by the pop.
    drive(&mut parent, ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::FirstInGroup(GroupId(0)) }), 0);
    assert_eq!(parent.state.values[mode_row], plx_platform::i18n::msg::settings_audio_always(),
        "the list's readout is the committed value, with no reload");
}

/// The local fields have no snapshot to publish: the list re-reads the session values on its tick.
#[test]
fn the_field_list_rereads_a_local_value_a_picker_committed() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("pref-parent-local");
    let previous = plx_media::route::quality();
    plx_media::route::restore_quality(Quality::Original);
    let mut parent = PreferencesPage::new(EntryId(0), Kind::Playback);
    let before = parent.state.values[0].clone();
    plx_media::route::restore_quality(Quality::P480);
    drive(&mut parent, tick(), 0);
    assert_ne!(parent.state.values[0], before);
    assert_eq!(parent.state.values[0], Quality::P480.label());
    plx_media::route::restore_quality(previous);
}

/// A Size/Position pick made anywhere (the player's Style pages publish the live value before
/// their write lands) shows in the list's cached read-outs on its next tick.
#[test]
fn the_field_list_rereads_a_subtitle_look_picked_elsewhere() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("pref-parent-look");
    let (size, position) = (plx_media::route::subtitle_size(), plx_media::route::subtitle_position());
    plx_media::route::restore_subtitle_size(plx_media::route::SubtitleSize::Medium);
    plx_media::route::restore_subtitle_position(plx_media::route::SubtitlePosition::Low);
    let mut parent = PreferencesPage::new(EntryId(0), Kind::Playback);
    let size_row = parent.form.index_of(&RowId::Field(PickerKind::SubtitleSize)).unwrap();
    let position_row = parent.form.index_of(&RowId::Field(PickerKind::SubtitlePosition)).unwrap();
    assert_eq!(parent.state.values[size_row], plx_platform::i18n::msg::settings_playback_subtitle_size_medium());

    plx_media::route::restore_subtitle_size(plx_media::route::SubtitleSize::Large);
    plx_media::route::restore_subtitle_position(plx_media::route::SubtitlePosition::High);
    drive(&mut parent, tick(), 0);
    assert_eq!(parent.state.values[size_row], plx_platform::i18n::msg::settings_playback_subtitle_size_large());
    assert_eq!(parent.state.values[position_row], plx_platform::i18n::msg::settings_playback_subtitle_position_high());
    plx_media::route::restore_subtitle_size(size);
    plx_media::route::restore_subtitle_position(position);
}

/// Every field belongs to exactly the page whose list shows it: the Audio & Subtitles page lists
/// its four account fields and nothing else, the Playback page the six local ones, so a picker's
/// `kind()` (its crumb, its copy, whether it loads the Plex account) can never disagree with the
/// list that opened it.
#[test]
fn every_field_reports_the_page_whose_list_shows_it() {
    let prefs = AudioPreferences::default();
    for kind in [Kind::Playback, Kind::AudioSubtitles] {
        let inputs = FieldListInputs { kind, quality: Quality::Original, direct_play: DirectPlayMode::Auto,
            prefs: Some(&prefs), busy: false, show_retry: false, read_only: false };
        for &field in fields_of(&inputs) {
            assert_eq!(field.kind(), Some(kind), "{field:?} is listed by {kind:?}");
        }
    }
    let audio = FieldListInputs { kind: Kind::AudioSubtitles, quality: Quality::Original, direct_play: DirectPlayMode::Auto,
        prefs: Some(&prefs), busy: false, show_retry: false, read_only: false };
    assert_eq!(fields_of(&audio), &[PickerKind::AudioLanguage, PickerKind::SubtitleMode, PickerKind::SubtitleLanguage, PickerKind::ForcedSubtitles]);
}

/// The Continue Watching row: shipped default is Open details, the picker lists the two options
/// with the live one checked, and choosing Play asks the app to save exactly that.
#[test]
fn the_continue_watching_row_defaults_to_open_details_and_its_picker_saves_the_pick() {
    use plx_media::route::DeckPress;
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("pref-deck-press");
    let prior = plx_media::route::deck_press();
    plx_media::route::restore_deck_press(DeckPress::Details);
    assert_eq!(plx_media::route::deck_press(), DeckPress::Details);

    let prefs = AudioPreferences::default();
    let readout = field_readout(PickerKind::DeckPress, Quality::Original, DirectPlayMode::Auto, Some(&prefs));
    let options = field_options(PickerKind::DeckPress, Quality::Original, DirectPlayMode::Auto, Some(&prefs));
    let current = resolve_value(PickerKind::DeckPress, Quality::Original, DirectPlayMode::Auto, Some(&prefs));
    assert_eq!(readout, "Open details");
    assert_eq!(current, Value::DeckPress(DeckPress::Details));
    assert_eq!(options, [("Open details".to_string(), Value::DeckPress(DeckPress::Details)),
        ("Play".to_string(), Value::DeckPress(DeckPress::Play))]);

    let mut picker = PickerPage::new(EntryId(0), PickerKind::DeckPress);
    let emitted = drive(&mut picker, ScreenEvent::Activate(1), 1);
    plx_media::route::restore_deck_press(prior);
    let saved = emitted.into_iter().find_map(|e| match e.fx {
        Fx::App(AppFx::Preferences(PreferenceCmd::Choose(plx_media::route::Choice::DeckPress(mode), _))) => Some(mode),
        _ => None,
    });
    assert_eq!(saved, Some(DeckPress::Play));
}

/// Seat a Plex Home profile called `title` on the fixture session. The caller holds
/// `testlock::serial()`.
fn seat_named(session: &plx_plex::plex::session::TempSession, uuid: &str, title: &str) {
    use plx_plex::plex::session::{current_gen, publish_seat_for_test, UserRef};
    let stamp = session.seated(uuid, "pref-read-only-account");
    publish_seat_for_test(Some(UserRef { uuid: uuid.into(), title: title.into(), ..Default::default() }),
        current_gen().wrapping_add(1), stamp);
}

/// Whether each listed row is drawn dim, in list order.
fn dims(page: &PreferencesPage) -> Vec<bool> {
    page.form.table.sections.iter().flat_map(|section| section.rows.iter().map(|row| row.dim)).collect()
}

fn right_at_edge() -> ScreenEvent<InnerHost> {
    ScreenEvent::Input(InputEvent { at: Tick::default(), source: plx_machine::machine::Source::Script,
        kind: InputKind::Key { key: Key::Right, sym: 0, wcode: 0, edge: plx_machine::machine::Edge::Down, at_edge: true } })
}

/// **While nobody is seated the Playback rows are read-only, and the page says why.** The Profile
/// settings belong to a seated profile and the data layer refuses a write for nobody, so a row that
/// opened a picker would offer a pick that cannot land. The rows stay on the page (showing the
/// values every profile starts from), dimmed (the form's `disabled` idiom: focusable, never activated), and OK / RIGHT on them
/// opens nothing. Audio & Subtitles (a Plex account preference, not a Profile row) is untouched.
#[test]
fn playback_rows_are_read_only_while_no_profile_is_seated() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("pref-read-only-unseated");
    let mut page = PreferencesPage::new(EntryId(0), Kind::Playback);
    assert_eq!(page.copy, plx_platform::i18n::msg::settings_playback_unseated(), "the page says to choose a profile");
    for field in [PickerKind::Quality, PickerKind::DirectPlay, PickerKind::SubtitleSize, PickerKind::SubtitlePosition,
        PickerKind::NextEpisode, PickerKind::DeckPress, PickerKind::SkipInterval, PickerKind::InstantDpadSeeking, PickerKind::DolbyVision] {
        let key = RowId::Field(field).key().0;
        let row = page.form.index_of(&RowId::Field(field)).expect("the row is still listed");
        assert!(dims(&page)[row], "{field:?}: dimmed");
        let ok = drive(&mut page, ScreenEvent::Activate(key), key);
        let right = drive(&mut page, right_at_edge(), key);
        assert!(pushed(&ok).is_empty() && pushed(&right).is_empty(), "{field:?}: OK and RIGHT open nothing");
        assert_eq!(preference_commands(&ok) + preference_commands(&right), 0, "{field:?}: and write nothing");
    }
    let account = Account::new("pref-read-only-audio", 31, AudioPreferences::default());
    let audio = loaded_audio_page(&account);
    assert!(!dims(&audio).is_empty() && dims(&audio).iter().all(|dim| !dim),
        "the account fields are not read-only");
}

/// **The Playback page names the profile whose settings it shows, and follows a seat change.** The
/// name is the profile chip's (the active profile's title). A change of the published profile moves
/// its generation, which rebuilds the copy and the rows on the next tick: the page never says "for
/// Maya" over Noah's values, a seated profile's rows open, and nobody seated is read-only again.
#[test]
fn the_playback_page_names_the_seated_profile_and_follows_the_seat() {
    let _serial = plx_base::testlock::serial();
    let session = plx_plex::plex::session::TempSession::new("pref-names-the-profile");
    let mut page = PreferencesPage::new(EntryId(0), Kind::Playback);
    assert_eq!(page.copy, plx_platform::i18n::msg::settings_playback_unseated());

    seat_named(&session, "maya", "Maya");
    drive(&mut page, tick(), 0);
    assert_eq!(page.copy, plx_platform::i18n::msg::settings_playback_copy_profile("Maya"));
    assert!(page.copy.contains("Maya"));
    let key = RowId::Field(PickerKind::Quality).key().0;
    assert_eq!(pushed(&drive(&mut page, ScreenEvent::Activate(key), key)), vec![SettingsPage::Picker(PickerKind::Quality)],
        "a seated profile's rows open");

    seat_named(&session, "noah", "Noah");
    drive(&mut page, tick(), 0);
    assert_eq!(page.copy, plx_platform::i18n::msg::settings_playback_copy_profile("Noah"), "the next profile is named");
    assert!(!page.copy.contains("Maya"));

    plx_plex::plex::session::publish_profile_for_test(None, plx_plex::plex::session::current_gen().wrapping_add(1));
    drive(&mut page, tick(), 0);
    assert_eq!(page.copy, plx_platform::i18n::msg::settings_playback_unseated(), "unseated again: read-only again");
    assert!(pushed(&drive(&mut page, ScreenEvent::Activate(key), key)).is_empty());
}
