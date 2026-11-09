//! The field-list pages. The choice list's own transaction tests live beside it
//! (`preferences/picker_tests.rs`); this file also owns the fixtures both share.
use super::*;
use plx_plex::plex::account::PreferenceRequest;
use plx_machine::machine::{FocusRead, InputOwner, PressRead, Stamped, Tick};
use plx_machine::present::Present;
use plx_ui::fixture::FixtureMeasure;

pub fn context(focus: u32) -> Cx<'static, InnerHost> {
    static MEASURE: FixtureMeasure = FixtureMeasure;
    Cx { views: plx_data::stores::browse::DirectoryView::empty_for_test().into(), tick: Tick::default(),
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
        (&mut playback, vec![PickerKind::Quality, PickerKind::DirectPlay, PickerKind::HdrOverlay, PickerKind::NextEpisode, PickerKind::DeckPress, PickerKind::SkipInterval, PickerKind::InstantDpadSeeking, PickerKind::DolbyVision, PickerKind::UriEngine]),
        (&mut audio, vec![PickerKind::AudioLanguage, PickerKind::SubtitleMode, PickerKind::SubtitleLanguage, PickerKind::ForcedSubtitles, PickerKind::SubtitleSize, PickerKind::SubtitlePosition]),
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
    let account = Account::new("pref-parent-look", 14, AudioPreferences::default());
    let mut parent = loaded_audio_page(&account);
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

/// Every field belongs to exactly the page whose list shows it: the Audio & subtitles page lists
/// its four account fields and the two Look rows (this app's own) and nothing else, the Video &
/// playback page the rest of the local ones, so a picker's
/// `kind()` (its crumb, its copy, whether it loads the Plex account) can never disagree with the
/// list that opened it.
#[test]
fn every_field_reports_the_page_whose_list_shows_it() {
    let prefs = AudioPreferences::default();
    for kind in [Kind::Playback, Kind::AudioSubtitles] {
        let inputs = FieldListInputs { kind, quality: Quality::Original, direct_play: DirectPlayMode::Auto,
            prefs: Some(&prefs), busy: false, show_retry: false, marker_prefs: Default::default(), read_only: false };
        for field in fields_of(&inputs) {
            assert_eq!(field.kind(), Some(kind), "{field:?} is listed by {kind:?}");
        }
    }
    let audio = FieldListInputs { kind: Kind::AudioSubtitles, quality: Quality::Original, direct_play: DirectPlayMode::Auto,
        prefs: Some(&prefs), busy: false, show_retry: false, marker_prefs: Default::default(), read_only: false };
    assert_eq!(fields_of(&audio), &[PickerKind::AudioLanguage, PickerKind::SubtitleMode, PickerKind::SubtitleLanguage, PickerKind::ForcedSubtitles, PickerKind::SubtitleSize, PickerKind::SubtitlePosition]);
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

/// The Playback engine row: the shipped default is Standard (the opt-in is off), the picker lists
/// the two options with the live one checked, and choosing Experimental asks the app to save exactly
/// that. The row's value is its readout, not its sentence.
#[test]
fn the_playback_engine_row_defaults_to_standard_and_its_picker_saves_the_opt_in() {
    use plx_plex::plex::session::UriEngine;
    let _serial = plx_base::testlock::serial();
    let _en = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _session = plx_plex::plex::session::TempSession::new("pref-uri-engine");
    let prior = plx_media::route::uri_engine();
    plx_media::route::restore_uri_engine(UriEngine::Off);

    let prefs = AudioPreferences::default();
    let readout = field_readout(PickerKind::UriEngine, Quality::Original, DirectPlayMode::Auto, Some(&prefs));
    let options = field_options(PickerKind::UriEngine, Quality::Original, DirectPlayMode::Auto, Some(&prefs));
    let current = resolve_value(PickerKind::UriEngine, Quality::Original, DirectPlayMode::Auto, Some(&prefs));
    assert_eq!(readout, "Standard");
    assert_eq!(current, Value::UriEngine(UriEngine::Off));
    assert_eq!(options, [("Standard".to_string(), Value::UriEngine(UriEngine::Off)),
        ("Experimental".to_string(), Value::UriEngine(UriEngine::On))]);
    assert_eq!(PickerKind::UriEngine.title(), "Playback engine");
    assert_eq!(PickerKind::UriEngine.parent(), SettingsPage::Playback);

    let mut picker = PickerPage::new(EntryId(0), PickerKind::UriEngine);
    let emitted = drive(&mut picker, ScreenEvent::Activate(1), 1);
    plx_media::route::restore_uri_engine(prior);
    let saved = emitted.into_iter().find_map(|e| match e.fx {
        Fx::App(AppFx::Preferences(PreferenceCmd::Choose(plx_media::route::Choice::UriEngine(engine), _))) => Some(engine),
        _ => None,
    });
    assert_eq!(saved, Some(UriEngine::On));
    // Opting in is visible on the row the picker returns to.
    plx_media::route::restore_uri_engine(UriEngine::On);
    assert_eq!(field_readout(PickerKind::UriEngine, Quality::Original, DirectPlayMode::Auto, Some(&prefs)), "Experimental");
    plx_media::route::restore_uri_engine(prior);
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
fn disabled_rows(page: &PreferencesPage) -> Vec<bool> {
    page.form.table.sections.iter().flat_map(|section| section.rows.iter().map(|row| row.disabled)).collect()
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
    for field in [PickerKind::Quality, PickerKind::DirectPlay, PickerKind::HdrOverlay,
        PickerKind::NextEpisode, PickerKind::DeckPress, PickerKind::SkipInterval, PickerKind::InstantDpadSeeking, PickerKind::DolbyVision, PickerKind::UriEngine] {
        let key = RowId::Field(field).key().0;
        let row = page.form.index_of(&RowId::Field(field)).expect("the row is still listed");
        assert!(disabled_rows(&page)[row], "{field:?}: disabled");
        let ok = drive(&mut page, ScreenEvent::Activate(key), key);
        let right = drive(&mut page, right_at_edge(), key);
        assert!(pushed(&ok).is_empty() && pushed(&right).is_empty(), "{field:?}: OK and RIGHT open nothing");
        assert_eq!(preference_commands(&ok) + preference_commands(&right), 0, "{field:?}: and write nothing");
    }
    let account = Account::new("pref-read-only-audio", 31, AudioPreferences::default());
    let mut audio = loaded_audio_page(&account);
    for field in [PickerKind::AudioLanguage, PickerKind::SubtitleMode, PickerKind::SubtitleLanguage, PickerKind::ForcedSubtitles] {
        let row = audio.form.index_of(&RowId::Field(field)).expect("the account row is listed");
        assert!(!disabled_rows(&audio)[row], "{field:?}: the account fields are not read-only");
    }
    // …but the Look rows are this app's own per-profile picks, read-only like the Playback rows.
    for field in [PickerKind::SubtitleSize, PickerKind::SubtitlePosition] {
        let key = RowId::Field(field).key().0;
        let row = audio.form.index_of(&RowId::Field(field)).expect("the Look row is still listed");
        assert!(disabled_rows(&audio)[row], "{field:?}: dimmed");
        let ok = drive(&mut audio, ScreenEvent::Activate(key), key);
        let right = drive(&mut audio, right_at_edge(), key);
        assert!(pushed(&ok).is_empty() && pushed(&right).is_empty(), "{field:?}: OK and RIGHT open nothing");
    }
}

/// **The Player HUD row is read-only with the rest of the Profile rows while nobody is seated**: the row stays on the
/// page, dimmed, and OK / RIGHT on it open nothing (the choice it opens is a pick the data layer would refuse).
/// With a profile seated the same row opens the Player HUD page.
#[test]
fn the_player_hud_row_is_read_only_while_no_profile_is_seated() {
    let _serial = plx_base::testlock::serial();
    let session = plx_plex::plex::session::TempSession::new("pref-hud-row-unseated");
    let mut page = PreferencesPage::new(EntryId(0), Kind::Playback);
    let key = RowId::PlayerHud.key().0;
    let row = page.form.index_of(&RowId::PlayerHud).expect("the Player HUD row is listed while unseated");
    assert!(disabled_rows(&page)[row], "disabled");
    let ok = drive(&mut page, ScreenEvent::Activate(key), key);
    let right = drive(&mut page, right_at_edge(), key);
    assert!(pushed(&ok).is_empty() && pushed(&right).is_empty(), "OK and RIGHT open nothing with no seat");

    seat_named(&session, "maya", "Maya");
    drive(&mut page, tick(), 0);
    let row = page.form.index_of(&RowId::PlayerHud).expect("the row is still listed once a profile is seated");
    assert!(!disabled_rows(&page)[row], "a seated profile's row is not disabled");
    assert_eq!(pushed(&drive(&mut page, ScreenEvent::Activate(key), key)), vec![SettingsPage::PlayerHud], "and OK opens the page");
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

#[test]
fn automatic_marker_settings_use_separate_grants_and_captured_scope() {
    let _guard = plx_base::testlock::serial();
    let mut inputs = FieldListInputs { kind: Kind::Playback, quality: Quality::Original,
        direct_play: DirectPlayMode::Auto, prefs: None, busy: false, show_retry: false,
        read_only: false, marker_prefs: Default::default() };
    assert!(!fields_of(&inputs).contains(&PickerKind::IntroSkip));
    assert!(!fields_of(&inputs).contains(&PickerKind::CreditsSkip));
    inputs.marker_prefs = MarkerPrefsView { scope: 9, intro_granted: true, ..Default::default() };
    assert!(fields_of(&inputs).contains(&PickerKind::IntroSkip));
    assert!(!fields_of(&inputs).contains(&PickerKind::CreditsSkip));
    assert_eq!(options_with_marker(PickerKind::IntroSkip, inputs.quality, inputs.direct_play, None, inputs.marker_prefs),
        MarkerSkipMode::LADDER.into_iter().map(|m| (marker_label(m).into(), Value::IntroSkip(m, 9))).collect::<Vec<_>>());
    inputs.marker_prefs.credits_granted = true;
    assert!(fields_of(&inputs).contains(&PickerKind::CreditsSkip));
    inputs.marker_prefs = Default::default();
    assert!(options_with_marker(PickerKind::IntroSkip, inputs.quality, inputs.direct_play, None, inputs.marker_prefs).is_empty());
}

// ---- Settings v2 stage 2: the groups, the rows each page lists and the sentences they state -------

fn inputs_of(kind: Kind, prefs: Option<&AudioPreferences>) -> FieldListInputs<'_> {
    FieldListInputs { kind, quality: Quality::Original, direct_play: DirectPlayMode::Auto, prefs, busy: false,
        show_retry: false, marker_prefs: Default::default(), read_only: false }
}

/// The headers of the groups a page draws and the rows under each, `(header, [fields])`, read off the
/// real form builder: the page as it stands, not the table the builder starts from.
fn groups_listed(inputs: &FieldListInputs<'_>) -> Vec<(String, Vec<PickerKind>)> {
    let mut form: FormTable<RowId, Action, SettingsPage> = FormTable::new(BAND);
    form.set(field_form(inputs), None);
    let mut out: Vec<(String, Vec<PickerKind>)> = Vec::new();
    for section in &form.table.sections {
        let fields: Vec<PickerKind> = (0..section.rows.len()).filter_map(|i| {
            let row = &section.rows[i];
            fields_of(inputs).into_iter().find(|f| f.title() == row.label)
        }).collect();
        out.push((section.header.to_string(), fields));
    }
    out
}

/// **Video & playback is three groups**: Video (Default quality, Direct Play, Dolby Vision), Player
/// (Skip interval, Instant D-pad seeking, HDR overlay, the marker skips when granted, and the Player HUD
/// row after them) and Up next (Next episode, Continue Watching). Subtitle size and position are gone.
#[test]
fn video_and_playback_lists_video_player_and_up_next() {
    use plx_platform::i18n::msg;
    let _serial = plx_base::testlock::serial();
    let _en = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let granted = MarkerPrefsView { scope: 1, intro_granted: true, credits_granted: true, ..Default::default() };
    let all = FieldListInputs { marker_prefs: granted, ..inputs_of(Kind::Playback, None) };
    assert_eq!(fields_of(&all), vec![
        PickerKind::Quality, PickerKind::DirectPlay, PickerKind::DolbyVision, PickerKind::UriEngine,
        PickerKind::SkipInterval, PickerKind::InstantDpadSeeking, PickerKind::HdrOverlay, PickerKind::IntroSkip, PickerKind::CreditsSkip,
        PickerKind::NextEpisode, PickerKind::DeckPress,
    ]);
    let groups = groups_listed(&all);
    assert_eq!(groups.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>(),
        [msg::settings_playback_group_video(), msg::settings_playback_group_player(), msg::settings_playback_group_up_next()]);
    assert_eq!(groups[2].1, vec![PickerKind::NextEpisode, PickerKind::DeckPress], "Up next");
    assert!(!fields_of(&all).contains(&PickerKind::SubtitleSize) && !fields_of(&all).contains(&PickerKind::SubtitlePosition),
        "subtitle size and position are on Audio & subtitles");
    assert_eq!(["Video", "Player", "Up next"], [msg::settings_playback_group_video(), msg::settings_playback_group_player(), msg::settings_playback_group_up_next()]);
}

/// The marker skips are rows only while their grants are held, each on its own grant, and they stand in
/// the Player group; the Player group never empties (the HUD row is always there).
#[test]
fn intro_and_credits_skip_rows_follow_their_grants_in_the_player_group() {
    let _serial = plx_base::testlock::serial();
    let none = inputs_of(Kind::Playback, None);
    assert_eq!(fields_of(&none), vec![
        PickerKind::Quality, PickerKind::DirectPlay, PickerKind::DolbyVision, PickerKind::UriEngine,
        PickerKind::SkipInterval, PickerKind::InstantDpadSeeking, PickerKind::HdrOverlay, PickerKind::NextEpisode, PickerKind::DeckPress]);
    let intro = FieldListInputs { marker_prefs: MarkerPrefsView { scope: 2, intro_granted: true, ..Default::default() }, ..inputs_of(Kind::Playback, None) };
    assert_eq!(fields_of(&intro).iter().filter(|f| matches!(f, PickerKind::IntroSkip | PickerKind::CreditsSkip)).collect::<Vec<_>>(), [&PickerKind::IntroSkip]);
    let both = FieldListInputs { marker_prefs: MarkerPrefsView { scope: 2, intro_granted: true, credits_granted: true, ..Default::default() }, ..inputs_of(Kind::Playback, None) };
    let order = fields_of(&both);
    let at = |f| order.iter().position(|x| *x == f).unwrap();
    assert!(at(PickerKind::HdrOverlay) < at(PickerKind::IntroSkip) && at(PickerKind::IntroSkip) < at(PickerKind::CreditsSkip) && at(PickerKind::CreditsSkip) < at(PickerKind::NextEpisode),
        "the skips stand in the Player group, ahead of Up next: {order:?}");
}

/// **Audio & subtitles is Audio, Subtitles and Look**: the account groups wait for the account, the Look
/// rows (this app's own) join them once the page has settled, and a loading page lists nothing.
#[test]
fn audio_and_subtitles_lists_audio_subtitles_and_look_once_the_page_has_settled() {
    use plx_platform::i18n::msg;
    let _serial = plx_base::testlock::serial();
    let prefs = AudioPreferences::default();
    let loaded = inputs_of(Kind::AudioSubtitles, Some(&prefs));
    let groups = groups_listed(&loaded);
    let headers: Vec<&str> = groups.iter().filter(|(_, f)| !f.is_empty()).map(|(h, _)| h.as_str()).collect();
    assert_eq!(headers, [msg::settings_audio_group_audio(), msg::settings_audio_group_subtitles(), msg::settings_audio_group_look()]);
    let by_header = |h: &str| groups.iter().find(|(x, _)| x == h).map(|(_, f)| f.clone()).unwrap();
    assert_eq!(by_header(msg::settings_audio_group_audio()), vec![PickerKind::AudioLanguage]);
    assert_eq!(by_header(msg::settings_audio_group_subtitles()), vec![PickerKind::SubtitleMode, PickerKind::SubtitleLanguage, PickerKind::ForcedSubtitles]);
    assert_eq!(by_header(msg::settings_audio_group_look()), vec![PickerKind::SubtitleSize, PickerKind::SubtitlePosition]);
    // loading: nothing listed, so the first landing seats the page's top row
    assert!(fields_of(&FieldListInputs { busy: true, show_retry: true, ..inputs_of(Kind::AudioSubtitles, None) }).is_empty());
    assert!(fields_of(&inputs_of(Kind::AudioSubtitles, None)).is_empty());
    // a failed load: Retry and the Look rows (they never needed the account)
    assert_eq!(fields_of(&FieldListInputs { show_retry: true, ..inputs_of(Kind::AudioSubtitles, None) }),
        vec![PickerKind::SubtitleSize, PickerKind::SubtitlePosition]);
}

/// The Retry button stands after the account groups and ahead of Look, and only for the account page.
#[test]
fn the_retry_button_stands_between_the_account_groups_and_look() {
    let _serial = plx_base::testlock::serial();
    let mut form: FormTable<RowId, Action, SettingsPage> = FormTable::new(BAND);
    form.set(field_form(&FieldListInputs { show_retry: true, ..inputs_of(Kind::AudioSubtitles, None) }), None);
    let retry = form.index_of(&RowId::Retry).expect("Retry is listed after a failed load");
    let size = form.index_of(&RowId::Field(PickerKind::SubtitleSize)).unwrap();
    assert!(retry < size);
    let mut playback: FormTable<RowId, Action, SettingsPage> = FormTable::new(BAND);
    playback.set(field_form(&FieldListInputs { show_retry: true, ..inputs_of(Kind::Playback, None) }), None);
    assert!(playback.index_of(&RowId::Retry).is_none(), "Video & playback has no account to retry");
}

/// Default quality reads "Overridden" while Direct Play is Forced (its sentence says why), and its own
/// quality otherwise.
#[test]
fn default_quality_reads_overridden_while_direct_play_is_forced() {
    use plx_platform::i18n::msg;
    let _serial = plx_base::testlock::serial();
    let _en = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    assert_eq!(field_readout(PickerKind::Quality, Quality::Original, DirectPlayMode::Forced, None), msg::settings_playback_overridden_value());
    assert_eq!(field_readout(PickerKind::Quality, Quality::Original, DirectPlayMode::Forced, None), "Overridden");
    assert_eq!(field_readout(PickerKind::Quality, Quality::Original, DirectPlayMode::Auto, None), Quality::Original.label());
    assert_ne!(field_readout(PickerKind::Quality, Quality::Original, DirectPlayMode::Auto, None), "Overridden");
}

/// **The sentence each field-list row states**: a Playback row and a Look row their own explanation,
/// an account row, the Retry button and the Player HUD the page's, a status the page's whatever has
/// focus, Default quality under Force the overridden sentence ahead of its own, and nobody seated the
/// page's (Playback) or the unseated sentence (Look).
#[test]
fn every_field_list_row_states_its_own_sentence() {
    use plx_platform::i18n::msg;
    let _serial = plx_base::testlock::serial();
    let page = "the page";
    let seated = Whose::Profile("Maya".into());
    let sentence = |kind, id: Option<&RowId>, status, force, whose: &Whose|
        row_sentence(kind, id, page, status, if force { DirectPlayMode::Forced } else { DirectPlayMode::Auto }, whose).into_owned();
    for field in [PickerKind::Quality, PickerKind::DirectPlay, PickerKind::DolbyVision, PickerKind::UriEngine, PickerKind::SkipInterval, PickerKind::InstantDpadSeeking,
        PickerKind::HdrOverlay, PickerKind::IntroSkip, PickerKind::CreditsSkip, PickerKind::NextEpisode, PickerKind::DeckPress] {
        assert_eq!(sentence(Kind::Playback, Some(&RowId::Field(field)), false, false, &seated), field.copy(), "{field:?}");
    }
    for field in [PickerKind::SubtitleSize, PickerKind::SubtitlePosition] {
        assert_eq!(sentence(Kind::AudioSubtitles, Some(&RowId::Field(field)), false, false, &seated), field.copy(), "{field:?}: the Look rows state their own");
        assert_eq!(sentence(Kind::AudioSubtitles, Some(&RowId::Field(field)), false, false, &Whose::Nobody), msg::settings_playback_unseated(),
            "{field:?}: nobody seated says a profile has to be chosen");
    }
    for field in [PickerKind::AudioLanguage, PickerKind::SubtitleMode, PickerKind::SubtitleLanguage, PickerKind::ForcedSubtitles] {
        assert_eq!(sentence(Kind::AudioSubtitles, Some(&RowId::Field(field)), false, false, &seated), page, "{field:?}: the account rows have none of their own");
    }
    for id in [RowId::Retry, RowId::PlayerHud] {
        assert_eq!(sentence(Kind::Playback, Some(&id), false, false, &seated), page, "{id:?}");
    }
    assert_eq!(sentence(Kind::Playback, None, false, false, &seated), page);
    assert_eq!(sentence(Kind::Playback, Some(&RowId::Field(PickerKind::NextEpisode)), true, false, &seated), page, "a status is the page's, whatever has focus");
    assert_eq!(sentence(Kind::Playback, Some(&RowId::Field(PickerKind::NextEpisode)), false, false, &Whose::Nobody), page, "Playback's page sentence says nobody is seated itself");
    let quality = sentence(Kind::Playback, Some(&RowId::Field(PickerKind::Quality)), false, true, &seated);
    assert_eq!(quality, format!("{} {}", msg::settings_playback_overridden(), PickerKind::Quality.copy()));
    assert_eq!(sentence(Kind::Playback, Some(&RowId::Field(PickerKind::DirectPlay)), false, true, &seated), PickerKind::DirectPlay.copy(),
        "Force changes only the Default quality row's meaning");
}
