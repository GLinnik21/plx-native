//! The choice list owns its transaction: a checked pick is a no-op pop, a write pops on a durable
//! receipt, a failure keeps the page with its status and a Retry, Force is asked before it is
//! persisted, and a stale request leaves rather than writing.
use super::*;
use super::super::tests::{drive, popped, preference_commands, tick, Account, context};
use plx_machine::machine::{Source, Stamped, Tick};

fn save_reply(emitted: Vec<Stamped<InnerHost>>) -> mpsc::Sender<AccountPreferenceReply> {
    for e in emitted {
        if let Fx::App(AppFx::Preferences(PreferenceCmd::Save { reply, .. })) = e.fx { return reply; }
    }
    panic!("expected a Save command");
}
fn key_of(page: &PickerPage, value: &Value) -> u32 {
    let i = page.form.index_of(&OptionId::Choice(value.clone())).expect("option present");
    page.form.key_at(i).unwrap().0
}
fn audio_picker(account: &Account, field: PickerKind) -> PickerPage {
    let _parent = super::super::tests::loaded_audio_page(account);
    PickerPage::new(EntryId(0), field)
}

#[test]
fn a_picker_opens_on_the_checked_option_with_position_keys() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("picker-opens-checked");
    let previous = crate::route::quality();
    crate::route::restore_quality(Quality::P480);
    let page = PickerPage::new(EntryId(0), PickerKind::Quality);
    let ladder = crate::route::available_quality_ladder();
    let at = ladder.iter().position(|q| *q == Quality::P480).unwrap();
    assert_eq!(page.state.selected, at as u32, "the cursor starts on the checked option");
    assert_eq!(page.form.key_at(at), Some(RowKey(at as u32)), "an option's key is its position");
    assert_eq!(page.state.checked, at as u32);
    crate::route::restore_quality(previous);
}

#[test]
fn choosing_the_checked_value_pops_without_a_write() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("picker-checked-noop");
    let previous = crate::route::quality();
    crate::route::restore_quality(Quality::P480);
    let mut page = PickerPage::new(EntryId(0), PickerKind::Quality);
    let key = page.state.selected;
    let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
    assert!(popped(&emitted));
    assert_eq!(preference_commands(&emitted), 0);
    assert!(page.txn.pending.is_none());
    crate::route::restore_quality(previous);
}

/// OK on the already-checked language is not a change and must not write to the account, least of
/// all the canonical `pt-BR` in place of the stored deprecated `pb`; a different one still does.
#[test]
fn picking_the_already_current_language_writes_nothing_and_a_different_one_still_does() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("picker-noop-language");
    let account = Account::new("picker-noop-language", 21, AudioPreferences { stated_language: Some("pb".into()), ..Default::default() });
    let mut page = audio_picker(&account, PickerKind::AudioLanguage);
    let same = key_of(&page, &Value::Language("pt-BR".into()));
    let emitted = drive(&mut page, ScreenEvent::Activate(same), same);
    assert_eq!(preference_commands(&emitted), 0, "a no-op pick must not emit a preference command");
    assert!(popped(&emitted));
    let other = key_of(&page, &Value::Language("fr".into()));
    let emitted = drive(&mut page, ScreenEvent::Activate(other), other);
    assert_eq!(preference_commands(&emitted), 1, "a different pick still saves");
    assert!(!popped(&emitted), "the page stays until the receipt is durable");
    assert!(page.state.io.busy);
}

#[test]
fn a_durable_account_receipt_pops_and_a_failed_one_keeps_the_page_with_retry() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("picker-receipt");
    let account = Account::new("picker-receipt", 22, AudioPreferences::default());
    let mut page = audio_picker(&account, PickerKind::SubtitleMode);
    let always = key_of(&page, &Value::Mode(2));
    // failure first
    let reply = save_reply(drive(&mut page, ScreenEvent::Activate(always), always));
    reply.send(AccountPreferenceReply { request: Some(account.request.clone()), outcome: Err(PreferenceError::Unavailable) }).unwrap();
    let emitted = drive(&mut page, tick(), always);
    assert!(!popped(&emitted), "a failed write keeps the picker");
    assert!(!page.state.io.status.is_empty(), "the failure is shown");
    assert!(!page.state.io.busy);
    let retry = page.form.index_of_key(RowKey(RETRY_KEY)).expect("a failed account write offers Retry");
    assert_eq!(page.form.table.sel, retry as i32, "the cursor lands on Retry");
    // Retry re-sends the SAME update, and its durable receipt pops
    let reply = save_reply(drive(&mut page, ScreenEvent::Activate(RETRY_KEY), RETRY_KEY));
    reply.send(account.reply(AudioPreferences { subtitle_mode: 2, ..Default::default() })).unwrap();
    let emitted = drive(&mut page, tick(), always);
    assert!(popped(&emitted), "the durable receipt pops the picker");
}

#[test]
fn a_local_write_pops_on_its_receipt_and_a_refusal_keeps_the_page() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("picker-local");
    let previous = crate::route::quality();
    crate::route::restore_quality(Quality::Original);
    for ok in [true, false] {
        let mut page = PickerPage::new(EntryId(0), PickerKind::Quality);
        let other = crate::route::available_quality_ladder().iter().position(|q| *q != Quality::Original).unwrap() as u32;
        let emitted = drive(&mut page, ScreenEvent::Activate(other), other);
        let Some(Fx::App(AppFx::Preferences(PreferenceCmd::Quality { reply, .. }))) = emitted.into_iter().map(|e| e.fx)
            .find(|f| matches!(f, Fx::App(AppFx::Preferences(_)))) else { panic!("a quality pick saves locally") };
        reply.send(ok).unwrap();
        let emitted = drive(&mut page, tick(), other);
        assert_eq!(popped(&emitted), ok);
        assert_eq!(page.state.io.status.is_empty(), ok);
    }
    crate::route::restore_quality(previous);
}

#[test]
fn force_requires_acknowledgement_and_cancel_never_saves() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("force-confirm-cancel");
    crate::route::restore_direct_play_mode(DirectPlayMode::Auto);
    let mut page = PickerPage::new(EntryId(0), PickerKind::DirectPlay);
    let forced = key_of(&page, &Value::DirectPlay(DirectPlayMode::Forced));
    let emitted = drive(&mut page, ScreenEvent::Activate(forced), forced);
    assert!(page.alert.is_open());
    assert!(!page.alert.choice(), "Cancel is the default answer");
    assert_eq!(preference_commands(&emitted), 0, "opening the warning must not persist Force");
    assert!(page.txn.pending.is_none());
    assert_eq!(crate::route::direct_play_mode(), DirectPlayMode::Auto);
    let cancel = ScreenEvent::Input(InputEvent { at: Tick::default(), source: Source::RemoteFifo,
        kind: InputKind::Key { key: Key::Back, edge: Edge::Down, sym: 0, wcode: 0, at_edge: false } });
    let emitted = drive(&mut page, cancel, ALERT);
    assert!(!page.alert.is_open());
    assert_eq!(preference_commands(&emitted), 0);
    assert!(page.txn.pending.is_none());
    assert_eq!(crate::route::direct_play_mode(), DirectPlayMode::Auto);
}

#[test]
fn confirming_force_emits_a_preference_effect_without_executing_it() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("force-confirm-yes");
    let previous = crate::route::direct_play_mode();
    crate::route::restore_direct_play_mode(DirectPlayMode::Auto);
    let mut page = PickerPage::new(EntryId(0), PickerKind::DirectPlay);
    let forced = key_of(&page, &Value::DirectPlay(DirectPlayMode::Forced));
    drive(&mut page, ScreenEvent::Activate(forced), forced);
    let emitted = drive(&mut page, ScreenEvent::PressCommit(plx_machine::machine::PressId(1)), ALERT + 1);
    assert!(page.state.io.busy);
    assert!(emitted.iter().any(|event| matches!(&event.fx,
        Fx::App(AppFx::Preferences(PreferenceCmd::DirectPlay { mode: DirectPlayMode::Forced, .. })))));
    assert_eq!(crate::route::direct_play_mode(), DirectPlayMode::Auto,
        "only an admitted app executor can persist and activate Force");
    crate::route::restore_direct_play_mode(previous);
}

/// A request captured for a profile that is no longer active must not be written through: the
/// picker leaves, and the field list beneath reloads.
#[test]
fn a_stale_request_leaves_the_picker_without_a_write_and_the_list_reloads() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("picker-stale");
    let account = Account::new("picker-stale", 23, AudioPreferences::default());
    let mut parent = super::super::tests::loaded_audio_page(&account);
    let mut page = PickerPage::new(EntryId(0), PickerKind::ForcedSubtitles);
    assert!(page.txn.snapshot.is_some(), "the picker starts from its parent's confirmed snapshot");
    account.go_stale();
    let emitted = drive(&mut page, tick(), 0);
    assert!(popped(&emitted));
    assert_eq!(preference_commands(&emitted), 0);
    let emitted = drive(&mut parent, tick(), 0);
    assert_eq!(emitted.iter().filter(|e| matches!(&e.fx, Fx::App(AppFx::Preferences(PreferenceCmd::Load { .. })))).count(), 1,
        "the field list drops its stale snapshot and loads again");
}

#[test]
fn back_is_held_while_an_account_write_is_in_flight() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("picker-back-held");
    let account = Account::new("picker-back-held", 24, AudioPreferences::default());
    let mut page = audio_picker(&account, PickerKind::ForcedSubtitles);
    let other = key_of(&page, &Value::Forced(1));
    drive(&mut page, ScreenEvent::Activate(other), other);
    let back = ScreenEvent::Input(InputEvent { at: Tick::default(), source: Source::RemoteFifo,
        kind: InputKind::Key { key: Key::Back, edge: Edge::Down, sym: 0, wcode: 0, at_edge: false } });
    let mut out = Vec::new(); let mut present = plx_machine::present::Present::new();
    let handled = page.step(&back, &context(other), &mut Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present));
    assert_eq!(handled, Handled::Yes, "BACK is swallowed until the receipt lands");
}

/// Subtitle Size and Position are Local-save pickers like Quality: the checked entry pops with no
/// write; another entry emits exactly its command and pops on the receipt.
#[test]
fn the_subtitle_pickers_write_locally_and_pop_on_the_receipt() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("picker-subtitle-local");
    let (size, position) = (crate::route::subtitle_size(), crate::route::subtitle_position());
    crate::route::restore_subtitle_size(crate::route::SubtitleSize::Medium);
    crate::route::restore_subtitle_position(crate::route::SubtitlePosition::Low);
    for field in [PickerKind::SubtitleSize, PickerKind::SubtitlePosition] {
        let mut page = PickerPage::new(EntryId(0), field);
        let checked = page.state.checked;
        let emitted = drive(&mut page, ScreenEvent::Activate(checked), checked);
        assert!(popped(&emitted), "{field:?}: the checked value pops");
        assert_eq!(preference_commands(&emitted), 0, "{field:?}: and writes nothing");

        let mut page = PickerPage::new(EntryId(0), field);
        let other = if checked == 0 { 1 } else { 0 };
        let emitted = drive(&mut page, ScreenEvent::Activate(other), other);
        assert!(!popped(&emitted), "{field:?}: a write waits for its receipt");
        let reply = emitted.into_iter().find_map(|e| match e.fx {
            Fx::App(AppFx::Preferences(PreferenceCmd::SubtitleSize { reply, .. }))
            | Fx::App(AppFx::Preferences(PreferenceCmd::SubtitlePosition { reply, .. })) => Some(reply),
            _ => None,
        }).expect("a local subtitle command");
        reply.send(true).unwrap();
        assert!(popped(&drive(&mut page, tick(), other)), "{field:?}: pops on the receipt");
    }
    crate::route::restore_subtitle_size(size);
    crate::route::restore_subtitle_position(position);
}

/// The optimistic path publishes the live value before the receipt: the open picker moves its
/// checkmark on the next tick instead of waiting for the write.
#[test]
fn the_size_picker_moves_its_checkmark_when_the_live_value_is_published() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("picker-optimistic-check");
    let (size, position) = (crate::route::subtitle_size(), crate::route::subtitle_position());
    crate::route::restore_subtitle_size(crate::route::SubtitleSize::Medium);
    let mut page = PickerPage::new(EntryId(0), PickerKind::SubtitleSize);
    let before = page.state.checked;
    crate::route::restore_subtitle_size(crate::route::SubtitleSize::Large);
    drive(&mut page, tick(), before);
    assert_ne!(page.state.checked, before, "the checked option follows the published value");
    crate::route::restore_subtitle_size(size);
    crate::route::restore_subtitle_position(position);
}

/// A failed Size write leaves the live value (and so the checkmark) on the new rung while the disk
/// keeps the old one. OK on that checked row must write again, not pop and strand the pick.
#[test]
fn ok_on_the_checked_size_after_a_failed_write_retries_the_write() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("picker-size-retry");
    let (size, position) = (crate::route::subtitle_size(), crate::route::subtitle_position());
    crate::route::restore_subtitle_size(crate::route::SubtitleSize::Medium);
    let mut page = PickerPage::new(EntryId(0), PickerKind::SubtitleSize);
    let other = if page.state.checked == 0 { 1 } else { 0 };
    let emitted = drive(&mut page, ScreenEvent::Activate(other), other);
    let reply = emitted.into_iter().find_map(|e| match e.fx {
        Fx::App(AppFx::Preferences(PreferenceCmd::SubtitleSize { size, reply })) => {
            // what select_subtitle_size does on the main thread before the write lands
            crate::route::restore_subtitle_size(size);
            Some(reply)
        }
        _ => None,
    }).expect("a size command");
    reply.send(false).unwrap();
    assert!(!popped(&drive(&mut page, tick(), other)), "a failed write keeps the page");
    assert_eq!(page.state.checked, other, "the live pick is the checked row");
    let emitted = drive(&mut page, ScreenEvent::Activate(other), other);
    assert!(!popped(&emitted), "OK on the unsaved checked row does not pop");
    assert_eq!(preference_commands(&emitted), 1, "it writes again");
    crate::route::restore_subtitle_size(size);
    crate::route::restore_subtitle_position(position);
}

/// Subtitle size and position are LOCAL per-television settings. Opening their picker must not
/// start the Plex account load (whose failure puts the Audio page's Retry row under the cursor),
/// must not read as the Audio & Subtitles page, and must not carry the account note.
#[test]
fn the_local_subtitle_pickers_never_touch_the_plex_account() {
    let _serial = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("pref-local-subtitle-pickers");
    for field in [PickerKind::SubtitleSize, PickerKind::SubtitlePosition] {
        let mut page = PickerPage::new(EntryId(0), field);
        let mut emitted = drive(&mut page, ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::ContainerGroup(GroupId(0)) }), 0);
        emitted.extend(drive(&mut page, tick(), 0));
        assert_eq!(preference_commands(&emitted), 0, "{field:?}: no account load");
        assert!(page.txn.pending.is_none(), "{field:?}: nothing in flight");
        assert!(!page.state.io.busy, "{field:?}: not busy");
        assert!(page.state.io.status.is_empty(), "{field:?}: no loading status");
        assert!(page.form.index_of_key(RowKey(RETRY_KEY)).is_none(), "{field:?}: no Retry row");
        assert!(!page.copy.contains(plx_platform::i18n::msg::settings_audio_account_note()), "{field:?}: no account note");
        assert_eq!(field.kind(), Kind::Playback, "{field:?} belongs to the Video & playback page");
        assert_eq!(field.kind().title(), plx_platform::i18n::msg::settings_playback_title(), "{field:?}: the crumb names the page it opened from");
    }
}
