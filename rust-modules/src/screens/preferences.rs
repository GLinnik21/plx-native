//! Playback defaults and Plex account preferences on the shared Settings table: the two field-list
//! pages (Playback, Audio & Subtitles) and, in [`picker`], the choice list each field pushes.
//!
//! **One navigation architecture.** A field row is a `Nav` item of the page's [`FormTable`]:
//! activating it pushes `SettingsPage::Picker(field)` through [`form_activate`], the family's one
//! activation path, so the surface's stack executor and its one push spring (`RouteSurface`'s
//! `Push`) carry a picker exactly as they carry Legal or Privacy. This page owns no submenu, no
//! second table and no `RoutePush`.
//!
//! **The picker owns its transaction.** [`PickerPage`] (a child module, so it shares this file's
//! private `Value`/option/readout helpers without a screen naming a sibling) loads or adopts the
//! confirmed snapshot, runs the Direct Play *Forced* acknowledgement, submits the write and pops on
//! a durable receipt. The field-list page keeps no pending write of its own for a pick; it learns
//! the new confirmed value through [`Txn::adopt_shared`] — the snapshot a landed load or save
//! publishes to the UI thread's shared cell — and Local values (quality, direct play) through the
//! session reads every tick already makes. The confirmed snapshot stays with the screens; workers
//! return receipts and never mutate the UI, and a failed save leaves the confirmed value intact.
use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver};
use crate::plex::account::{AudioPreferences, PreferenceError, PreferenceRequest, PreferenceSnapshot, PreferenceUpdate};
use crate::route::{DirectPlayMode, NextEpisodeMode, Quality, SkipInterval, SubtitlePosition, SubtitleSize};
use crate::ui::form::{Form, FormId, FormSection, FormTable, RowKey, RowKind};
use crate::ui::frame::Budget;
use crate::ui::machine::{Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId,
    Handled, InputEvent, InputKind, InstanceId, Key, LogicalState, Machine, MachineId};
use crate::ui::screen::{DrawFrame, Enter, FocusSource, FocusTarget, HitSource, RenderStrategy,
    Screen, ScreenEvent};
use crate::ui::table::Row;
use crate::ui::table_screen::{Header, TableScreen};
use crate::ui::route_screen::RouteLayout;
use crate::ui::{theme, Rect};
use super::family::{form_activate, form_focus, form_right_target, InnerHost, PickerKind, SettingsPage};
use super::registry::{word, AccountPreferenceReply, AppFx, PreferenceCmd, BAND};

mod picker;
pub(crate) use picker::{PickerPage, SHAPE as PICKER_SHAPE};

pub(crate) const SHAPE: &str = "PreferencesV3{kind:u8,selection:u32,busy:bool,status:str,quality:u8,direct_play:u8,values:[str]}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind { Playback, AudioSubtitles }
impl PickerKind {
    fn title(self) -> &'static str { match self {
        Self::Quality => crate::i18n::msg::settings_playback_quality(), Self::DirectPlay => crate::i18n::msg::settings_playback_direct_play(),
        Self::SubtitleSize => crate::i18n::msg::settings_playback_subtitle_size(), Self::SubtitlePosition => crate::i18n::msg::settings_playback_subtitle_position(),
        Self::NextEpisode => crate::i18n::msg::settings_playback_next_episode(),
        Self::SkipInterval => crate::i18n::msg::settings_playback_skip_interval(),
        Self::AudioLanguage => crate::i18n::msg::settings_audio_language(), Self::SubtitleMode => crate::i18n::msg::settings_audio_subtitles(),
        Self::SubtitleLanguage => crate::i18n::msg::settings_audio_subtitle_language(), Self::ForcedSubtitles => crate::i18n::msg::settings_audio_forced_subtitles(),
    }}
    /// What this field's picker says about it under its title. The four account fields share the
    /// account note (it is about where those values are saved); each local one has its own.
    fn copy(self) -> &'static str { match self {
        Self::Quality => crate::i18n::msg::settings_playback_quality_copy(),
        Self::DirectPlay => crate::i18n::msg::settings_playback_direct_play_copy(),
        Self::SubtitleSize => crate::i18n::msg::settings_playback_subtitle_size_copy(),
        Self::SubtitlePosition => crate::i18n::msg::settings_playback_subtitle_position_copy(),
        Self::NextEpisode => crate::i18n::msg::settings_playback_next_episode_copy(),
        Self::SkipInterval => crate::i18n::msg::settings_playback_skip_interval_copy(),
        Self::AudioLanguage | Self::SubtitleMode | Self::SubtitleLanguage | Self::ForcedSubtitles => crate::i18n::msg::settings_audio_account_note(),
    }}
    /// The field-list page this field belongs to.
    fn kind(self) -> Kind {
        match self {
            Self::Quality | Self::DirectPlay | Self::SubtitleSize | Self::SubtitlePosition | Self::NextEpisode | Self::SkipInterval => Kind::Playback,
            Self::AudioLanguage | Self::SubtitleMode | Self::SubtitleLanguage | Self::ForcedSubtitles => Kind::AudioSubtitles,
        }
    }
}
impl Kind {
    fn title(self) -> &'static str {
        match self {
            Kind::Playback => crate::i18n::msg::settings_playback_title(), Kind::AudioSubtitles => crate::i18n::msg::settings_audio_title(),
        }
    }
    fn word(self) -> &'static str {
        match self { Kind::Playback => word::PLAYBACK, Kind::AudioSubtitles => word::AUDIO }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value { Quality(Quality), DirectPlay(DirectPlayMode), SubtitleSize(SubtitleSize), SubtitlePosition(SubtitlePosition), NextEpisode(NextEpisodeMode), SkipInterval(SkipInterval), Language(String), Mode(i64), Forced(i64) }

/// The hashed half of a transaction: what the page shows while a load or save is in flight.
struct Io { busy: bool, status: String }

/// What the copy under a title explains: a field-list page, or one field's picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Subject { Page(Kind), Picker(PickerKind) }
impl Subject {
    /// Whether Force Direct Play changes what this page is about: the Playback list itself, and the
    /// two pickers whose outcome it overrides (Quality is pinned to Original, Direct Play is the
    /// setting). The other four Playback pickers are untouched by it.
    fn overridden_by_force(self) -> bool {
        matches!(self, Self::Page(Kind::Playback) | Self::Picker(PickerKind::Quality | PickerKind::DirectPlay))
    }
}

/// The copy under the title: the failure / progress status (with the Force note while Forced is
/// on and matters here), else the Force note itself, else the subject's own explanation — a
/// picker explains its own setting, never its page's.
fn copy_text<'a>(subject: Subject, status: &'a str, direct_play: DirectPlayMode) -> Cow<'a, str> {
    let forced = direct_play == DirectPlayMode::Forced && subject.overridden_by_force();
    if !status.is_empty() {
        return if forced {
            Cow::Owned(format!("{}\n\n{}", status, crate::i18n::msg::settings_playback_force_note()))
        } else { Cow::Borrowed(status) };
    }
    if forced { return Cow::Borrowed(crate::i18n::msg::settings_playback_force_note()); }
    Cow::Borrowed(match subject {
        Subject::Page(Kind::AudioSubtitles) => crate::i18n::msg::settings_audio_account_note(),
        Subject::Page(Kind::Playback) => crate::i18n::msg::settings_playback_copy(),
        Subject::Picker(field) => field.copy(),
    })
}

enum Pending {
    Account(Receiver<AccountPreferenceReply>),
    Local(Receiver<bool>),
}

thread_local! {
    /// The last confirmed account snapshot a landed load or save published, with a generation that
    /// moves on every publish. The field list and its pickers are pages of ONE stack on the UI
    /// thread; this is how a picker starts from the snapshot its parent confirmed (no second
    /// load under a sliding page) and how the parent learns what the picker's save confirmed.
    static CONFIRMED: RefCell<(u64, Option<(PreferenceRequest, PreferenceSnapshot)>)> = const { RefCell::new((0, None)) };
}

/// A load/save in flight plus the confirmed account snapshot it edits — one implementation for
/// the field list (initial load, Retry) and the pickers (writes).
struct Txn {
    request: Option<PreferenceRequest>,
    snapshot: Option<PreferenceSnapshot>,
    pending: Option<Pending>,
    retry: Option<PreferenceUpdate>,
    /// The pending receipt answers a WRITE rather than a load.
    saving: bool,
    /// The [`CONFIRMED`] generation this transaction has already taken.
    seen: u64,
}
/// What one [`Txn::poll`] found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Landed { Nothing, Changed, Saved }

impl Txn {
    fn new(adopt_existing: bool) -> Self {
        let mut txn = Self { request: None, snapshot: None, pending: None, retry: None, saving: false,
            seen: if adopt_existing { 0 } else { CONFIRMED.with(|c| c.borrow().0) } };
        txn.adopt_shared();
        txn
    }
    fn prefs(&self) -> Option<&AudioPreferences> { self.snapshot.as_ref().map(|s| &s.preferences) }
    fn stale(&self) -> bool { self.request.as_ref().is_some_and(|r| !r.is_current()) }
    /// Publish the confirmed snapshot for the other page of the pair.
    fn publish(&mut self) {
        let (Some(request), Some(snapshot)) = (self.request.clone(), self.snapshot.clone()) else { return; };
        self.seen = CONFIRMED.with(|c| {
            let mut c = c.borrow_mut();
            c.0 = c.0.wrapping_add(1);
            c.1 = Some((request, snapshot));
            c.0
        });
    }
    /// Take a newer published snapshot, unless a load/save of our own is in flight. True when taken.
    fn adopt_shared(&mut self) -> bool {
        if self.pending.is_some() { return false; }
        let taken = CONFIRMED.with(|c| {
            let c = c.borrow();
            (c.0 != self.seen).then(|| (c.0, c.1.clone()))
        });
        let Some((generation, shared)) = taken else { return false; };
        self.seen = generation;
        match shared {
            Some((request, snapshot)) if request.is_current() => {
                self.request = Some(request); self.snapshot = Some(snapshot); self.retry = None; true
            }
            _ => false,
        }
    }
    fn load(&mut self, io: &mut Io, fx: &mut Effects<'_, InnerHost>) {
        self.request = None;
        self.retry = None;
        self.start_account(io, None, fx);
    }
    fn start_account(&mut self, io: &mut Io, update: Option<PreferenceUpdate>, fx: &mut Effects<'_, InnerHost>) {
        if self.pending.is_some() { return; }
        self.retry = update.clone();
        let (reply, rx) = mpsc::channel();
        let command = match (update, self.request.clone(), self.snapshot.clone()) {
            (Some(update), Some(request), Some(base)) => PreferenceCmd::Save { request, base, update, reply },
            _ => PreferenceCmd::Load { reply },
        };
        self.saving = matches!(&command, PreferenceCmd::Save { .. });
        io.status = if self.saving {
            crate::i18n::msg::settings_audio_saving()
        } else { crate::i18n::msg::settings_audio_loading() }.into();
        self.pending = Some(Pending::Account(rx)); io.busy = true;
        fx.push(Fx::App(AppFx::Preferences(command)));
    }
    fn save_local(&mut self, io: &mut Io, value: Value, fx: &mut Effects<'_, InnerHost>) {
        let (reply, rx) = mpsc::channel();
        let command = match value {
            Value::Quality(quality) => PreferenceCmd::Quality { quality, reply },
            Value::DirectPlay(mode) => PreferenceCmd::DirectPlay { mode, reply },
            Value::SubtitleSize(size) => PreferenceCmd::SubtitleSize { size, reply },
            Value::SubtitlePosition(position) => PreferenceCmd::SubtitlePosition { position, reply },
            Value::NextEpisode(mode) => PreferenceCmd::NextEpisode { mode, reply },
            Value::SkipInterval(interval) => PreferenceCmd::SkipInterval { interval, reply },
            _ => return,
        };
        self.pending = Some(Pending::Local(rx)); io.busy = true; self.saving = true;
        io.status = crate::i18n::msg::settings_playback_saving().into();
        fx.push(Fx::App(AppFx::Preferences(command)));
    }
    /// The request no longer names the active profile: drop the snapshot and any in-flight
    /// receipt and load again.
    fn recover_stale(&mut self, io: &mut Io, fx: &mut Effects<'_, InnerHost>) {
        self.snapshot = None; self.pending = None; io.busy = false;
        self.load(io, fx);
    }
    fn poll(&mut self, io: &mut Io, fx: &mut Effects<'_, InnerHost>) -> Landed {
        enum Receipt { Account(AccountPreferenceReply), Local(bool), Failed }
        let receipt = match &self.pending {
            Some(Pending::Account(rx)) => match rx.try_recv() {
                Ok(result) => Some(Receipt::Account(result)),
                Err(mpsc::TryRecvError::Disconnected) => Some(Receipt::Failed), _ => None,
            },
            Some(Pending::Local(ticket)) => match ticket.try_recv() {
                Ok(result) => Some(Receipt::Local(result)),
                Err(mpsc::TryRecvError::Disconnected) => Some(Receipt::Failed), _ => None,
            },
            None => None,
        };
        let Some(receipt) = receipt else { return Landed::Nothing; };
        self.pending = None; io.busy = false;
        let was_write = std::mem::take(&mut self.saving);
        let mut landed = Landed::Changed;
        match receipt {
            Receipt::Account(reply) => {
                if reply.request.as_ref().is_some_and(|request| !request.is_current()) {
                    self.snapshot = None; self.load(io, fx);
                } else if let Some(request) = reply.request {
                    self.request = Some(request);
                    match reply.outcome {
                        Ok(snapshot) => {
                            self.snapshot = Some(snapshot); self.retry = None; io.status.clear();
                            self.publish();
                            if was_write { landed = Landed::Saved; }
                        }
                        Err(error) => {
                            if error == PreferenceError::Stale { self.retry = None; }
                            io.status = crate::i18n::msg::settings_audio_error_retry(error.message());
                        }
                    }
                } else {
                    self.request = None; self.snapshot = None; self.retry = None;
                    io.status = crate::i18n::msg::settings_audio_sign_in_again().into();
                }
            }
            Receipt::Local(true) => { io.status.clear(); if was_write { landed = Landed::Saved; } }
            Receipt::Local(false) | Receipt::Failed => io.status = crate::i18n::msg::settings_playback_save_failed().into(),
        }
        landed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowId { Field(PickerKind), Retry }
impl FormId for RowId {
    /// Hand-assigned, per page: each field's position in ITS page's list (what its table index was
    /// before the page moved onto a form, so a recorded focus on a field row is unchanged), Retry
    /// after them. Keys only need to be unique within one page.
    fn key(&self) -> RowKey {
        RowKey(match self {
            Self::Field(PickerKind::Quality | PickerKind::AudioLanguage) => 0,
            Self::Field(PickerKind::DirectPlay | PickerKind::SubtitleMode) => 1,
            Self::Field(PickerKind::SubtitleSize | PickerKind::SubtitleLanguage) => 2,
            Self::Field(PickerKind::SubtitlePosition | PickerKind::ForcedSubtitles) => 3,
            Self::Field(PickerKind::NextEpisode) => 4,
            Self::Field(PickerKind::SkipInterval) => 5,
            Self::Retry => 6,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action { Open, Retry }

struct State {
    kind: Kind, selected: u32, io: Io,
    quality: Quality, direct_play: DirectPlayMode, values: Vec<String>,
}
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.u8(self.kind as u8).u32(self.selected).bool(self.io.busy).str(&self.io.status)
            .u8(self.quality as u8).u8(self.direct_play as u8).u32(self.values.len() as u32);
        for v in &self.values { c.str(v); }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("preferences {:?} sel={} busy={}", self.kind, self.selected, self.io.busy));
    }
}

pub(crate) struct PreferencesPage {
    entry: EntryId,
    form: FormTable<RowId, Action, SettingsPage>,
    state: State,
    copy: String,
    txn: Txn,
    /// The caption Size/Position the rows were last built from. Not canonical state: it only
    /// notices a pick made elsewhere (the player's Style pages, or this page's picker, which
    /// publish the live value before their write lands) so the cached read-outs are rebuilt.
    look: (SubtitleSize, SubtitlePosition),
    /// The next-episode mode and skip interval the rows were last built from (not canonical
    /// state, like `look`).
    next_episode: NextEpisodeMode,
    skip_interval: SkipInterval,
}
impl PreferencesPage {
    pub(crate) fn new(entry: EntryId, kind: Kind) -> Self {
        let mut s = Self { entry, form: FormTable::new(BAND),
            state: State { kind, selected: 0, io: Io { busy: false, status: String::new() },
                quality: crate::route::quality(), direct_play: crate::route::direct_play_mode(), values: Vec::new() },
            copy: String::new(), txn: Txn::new(false),
            look: (crate::route::subtitle_size(), crate::route::subtitle_position()),
            next_episode: crate::route::next_episode_mode(), skip_interval: crate::route::skip_interval() };
        s.rebuild(None);
        s
    }
    fn view(&self) -> TableScreen<'_> {
        TableScreen::new(Header::new(RouteLayout::screen(), Some(crate::i18n::msg::settings_title()),
            self.state.kind.title(), &self.copy), &self.form.table, GroupId(0), self.entry).keyed(&self.form)
    }
    fn focus(&self, fx: &mut Effects<'_, InnerHost>) {
        let key = self.form.key_at(self.form.table.sel.max(0) as usize).map_or(0, |k| k.0);
        fx.push(Fx::Deliver(MachineId::Instance(InstanceId(0)),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(FocusKey { entry: self.entry, elem: key }) }))));
    }
    fn start_initial_load(&mut self, fx: &mut Effects<'_, InnerHost>) -> bool {
        if self.state.kind == Kind::AudioSubtitles && self.txn.request.is_none()
            && self.txn.snapshot.is_none() && self.txn.pending.is_none() && self.state.io.status.is_empty() {
            self.txn.load(&mut self.state.io, fx);
            true
        } else { false }
    }
    /// Rebuild the field list from the confirmed values, keeping the row the cursor is on by identity.
    fn rebuild(&mut self, keep: Option<RowId>) {
        self.state.quality = crate::route::quality();
        self.state.direct_play = crate::route::direct_play_mode();
        self.look = (crate::route::subtitle_size(), crate::route::subtitle_position());
        self.next_episode = crate::route::next_episode_mode();
        self.skip_interval = crate::route::skip_interval();
        self.copy = copy_text(Subject::Page(self.state.kind), &self.state.io.status, self.state.direct_play).into_owned();
        let inputs = FieldListInputs {
            kind: self.state.kind, quality: self.state.quality, direct_play: self.state.direct_play,
            prefs: self.txn.prefs(), busy: self.state.io.busy, show_retry: !self.state.io.status.is_empty(),
        };
        self.state.values = fields_of(&inputs).iter()
            .map(|&f| field_readout(f, inputs.quality, inputs.direct_play, inputs.prefs)).collect();
        self.form.table.compact = false; self.form.table.header_ink = theme::TEXT_READING; self.form.table.list_focused = true;
        self.form.set(field_form(&inputs), keep.as_ref());
        self.state.selected = self.form.key_at(self.form.table.sel.max(0) as usize).map_or(0, |k| k.0);
    }
    fn rebuild_keeping(&mut self) {
        let keep = self.form.selected_id().copied();
        self.rebuild(keep);
    }
    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        if self.state.io.busy { return; }
        match form_activate(&self.form, key, fx) {
            Some(Action::Retry) => {
                if let Some(update) = self.txn.retry.clone() { self.txn.start_account(&mut self.state.io, Some(update), fx); }
                else { self.txn.load(&mut self.state.io, fx); }
                self.rebuild_keeping();
                fx.invalidate(crate::ui::present::Provenance::Input);
            }
            Some(Action::Open) | None => {}
        }
    }
}

/// Which fields `kind`'s list shows: Audio & Subtitles has none until a snapshot has loaded.
fn fields_of(inputs: &FieldListInputs<'_>) -> &'static [PickerKind] {
    match inputs.kind {
        Kind::Playback => &[PickerKind::Quality, PickerKind::DirectPlay, PickerKind::SubtitleSize, PickerKind::SubtitlePosition, PickerKind::NextEpisode, PickerKind::SkipInterval],
        Kind::AudioSubtitles if inputs.prefs.is_some() => &[PickerKind::AudioLanguage, PickerKind::SubtitleMode, PickerKind::SubtitleLanguage, PickerKind::ForcedSubtitles],
        _ => &[],
    }
}
/// `PickerKind`'s current [`Value`] from the two locally-saved settings plus the account preferences, if
/// loaded. Free rather than a page method so [`field_section`] needs no page and no globals.
fn resolve_value(field: PickerKind, quality: Quality, direct_play: DirectPlayMode, prefs: Option<&AudioPreferences>) -> Value {
    match field {
        PickerKind::Quality => Value::Quality(quality),
        PickerKind::DirectPlay => Value::DirectPlay(direct_play),
        PickerKind::SubtitleSize => Value::SubtitleSize(crate::route::subtitle_size()),
        PickerKind::SubtitlePosition => Value::SubtitlePosition(crate::route::subtitle_position()),
        PickerKind::NextEpisode => Value::NextEpisode(crate::route::next_episode_mode()),
        PickerKind::SkipInterval => Value::SkipInterval(crate::route::skip_interval()),
        // A deprecated code (`pb`) resolves to its replacement so the picker checks that entry.
        PickerKind::AudioLanguage => Value::Language(prefs.and_then(|p| p.stated_language.as_deref()).map(crate::plex::languages::canonical).unwrap_or_default().to_string()),
        PickerKind::SubtitleLanguage => Value::Language(prefs.and_then(|p| p.subtitle_language.as_deref()).map(crate::plex::languages::canonical).unwrap_or_default().to_string()),
        PickerKind::SubtitleMode => Value::Mode(prefs.map_or(0, |p| p.subtitle_mode)),
        PickerKind::ForcedSubtitles => Value::Forced(prefs.map_or(0, |p| p.subtitle_forced)),
    }
}
/// `PickerKind`'s picker options.
fn field_options(field: PickerKind, quality: Quality, direct_play: DirectPlayMode, prefs: Option<&AudioPreferences>) -> Vec<(String, Value)> {
    match field {
        PickerKind::Quality => crate::route::available_quality_ladder().iter()
            .map(|q| (q.label().into(), Value::Quality(*q))).collect(),
        PickerKind::DirectPlay => [DirectPlayMode::Auto, DirectPlayMode::Forced, DirectPlayMode::Disabled]
            .into_iter().map(|m| (mode_label(m).into(), Value::DirectPlay(m))).collect(),
        PickerKind::SubtitleSize => SubtitleSize::LADDER.into_iter()
            .map(|s| (subtitle_size_label(s).into(), Value::SubtitleSize(s))).collect(),
        PickerKind::SubtitlePosition => SubtitlePosition::LADDER.into_iter()
            .map(|p| (subtitle_position_label(p).into(), Value::SubtitlePosition(p))).collect(),
        PickerKind::NextEpisode => NextEpisodeMode::LADDER.into_iter()
            .map(|m| (next_episode_label(m).into(), Value::NextEpisode(m))).collect(),
        PickerKind::SkipInterval => SkipInterval::LADDER.into_iter()
            .map(|i| (skip_interval_label(i), Value::SkipInterval(i))).collect(),
        PickerKind::SubtitleMode => [(crate::i18n::msg::settings_audio_manual(), 0), (crate::i18n::msg::settings_audio_foreign(), 1), (crate::i18n::msg::settings_audio_always(), 2)]
            .into_iter().map(|(label, mode)| (label.into(), Value::Mode(mode))).collect(),
        PickerKind::ForcedSubtitles => [crate::i18n::msg::settings_audio_prefer_regular(), crate::i18n::msg::settings_audio_prefer_forced(), crate::i18n::msg::settings_audio_only_forced(), crate::i18n::msg::settings_audio_only_regular()]
            .into_iter().enumerate().map(|(i, s)| (s.into(), Value::Forced(i as i64))).collect(),
        PickerKind::AudioLanguage | PickerKind::SubtitleLanguage => {
            let mut result = vec![(if field == PickerKind::AudioLanguage { crate::i18n::msg::settings_audio_original() } else { crate::i18n::msg::settings_audio_no_preference() }.into(), Value::Language(String::new()))];
            result.extend(crate::plex::languages::picker()
                .map(|l| (l.name.to_string(), Value::Language(l.code.to_string()))));
            let current = resolve_value(field, quality, direct_play, prefs);
            if !result.iter().any(|(_, v)| *v == current) {
                if let Value::Language(code) = current { result.push((code.clone(), Value::Language(code))); }
            }
            result
        }
    }
}
/// The trailing read-out `PickerKind`'s row shows for its current value. Direct Play and Forced
/// Subtitles show a short form of the picker's label ([`direct_play_readout`], [`forced_readout`]),
/// as does the Subtitles mode row's foreign-audio option; every other field shows the picker's own
/// label.
fn field_readout(field: PickerKind, quality: Quality, direct_play: DirectPlayMode, prefs: Option<&AudioPreferences>) -> String {
    match field {
        PickerKind::DirectPlay => direct_play_readout(direct_play).into(),
        PickerKind::ForcedSubtitles => forced_readout(prefs.map_or(0, |p| p.subtitle_forced)).into(),
        // Only the foreign-audio mode has a short form (es does not fit beside the label); the
        // other two modes show the picker's own label.
        PickerKind::SubtitleMode if prefs.is_some_and(|p| p.subtitle_mode == 1) => crate::i18n::msg::settings_audio_foreign_short().into(),
        PickerKind::SubtitleSize => subtitle_size_label(crate::route::subtitle_size()).into(),
        PickerKind::SubtitlePosition => subtitle_position_label(crate::route::subtitle_position()).into(),
        _ => {
            let current = resolve_value(field, quality, direct_play, prefs);
            field_options(field, quality, direct_play, prefs).into_iter().find(|(_, v)| *v == current)
                .map_or_else(|| crate::i18n::msg::settings_audio_not_set().into(), |(s, _)| s)
        }
    }
}

/// The inputs [`field_form`] builds a `Kind`'s field list from, so the builder reads no global.
struct FieldListInputs<'a> {
    kind: Kind,
    quality: Quality,
    direct_play: DirectPlayMode,
    /// The signed-in account's audio/subtitle preferences, once a snapshot has loaded.
    prefs: Option<&'a AudioPreferences>,
    busy: bool,
    /// Show the Retry row: the account kind has a non-empty status message and nothing is in flight.
    show_retry: bool,
}
/// The page's field list as a [`Form`]: each field a `Nav` item onto its picker page, plus the
/// Audio & Subtitles Retry button, built from [`FieldListInputs`] alone.
fn field_form(inputs: &FieldListInputs<'_>) -> Form<RowId, Action, SettingsPage> {
    let mut section = FormSection::new("");
    for &field in fields_of(inputs) {
        let value = field_readout(field, inputs.quality, inputs.direct_play, inputs.prefs);
        let mut row = Row::new(field.title()).value(&value).chevron(true).dim(inputs.busy);
        if field == PickerKind::Quality && inputs.direct_play == DirectPlayMode::Forced {
            row = row.detail(crate::i18n::msg::settings_playback_overridden());
        }
        if field == PickerKind::AudioLanguage && inputs.prefs.is_some_and(|p| p.auto_select_audio == Some(false)) {
            row = row.detail(crate::i18n::msg::settings_audio_selection_off());
        }
        section = section.item(RowId::Field(field), RowKind::Nav(SettingsPage::Picker(field)), Action::Open, row);
    }
    section = section.item_if(inputs.kind == Kind::AudioSubtitles && !inputs.busy && inputs.show_retry,
        RowId::Retry, RowKind::Button, Action::Retry,
        Row::new(crate::i18n::msg::settings_audio_retry()).detail(crate::i18n::msg::settings_audio_retry_detail()));
    Form::new().section(section)
}
fn mode_label(mode: DirectPlayMode) -> &'static str {
    match mode { DirectPlayMode::Auto => crate::i18n::msg::settings_playback_auto(), DirectPlayMode::Forced => crate::i18n::msg::settings_playback_forced(), DirectPlayMode::Disabled => crate::i18n::msg::settings_playback_disabled() }
}
use crate::appkit::track_menu::{subtitle_position_label, subtitle_size_label};
fn next_episode_label(mode: NextEpisodeMode) -> &'static str {
    match mode {
        NextEpisodeMode::Countdown => crate::i18n::msg::settings_playback_next_episode_countdown(),
        NextEpisodeMode::AfterCredits => crate::i18n::msg::settings_playback_next_episode_after_credits(),
        NextEpisodeMode::Off => crate::i18n::msg::settings_playback_next_episode_off(),
    }
}
fn skip_interval_label(interval: SkipInterval) -> String {
    crate::i18n::msg::settings_playback_skip_interval_seconds(interval.seconds())
}
/// The Direct Play row's trailing read-out. `mode_label`'s Forced string is long enough to squeeze
/// the row's label, so Forced alone takes the short form; the picker lists the full strings.
fn direct_play_readout(mode: DirectPlayMode) -> &'static str {
    match mode {
        DirectPlayMode::Forced => crate::i18n::msg::settings_playback_forced_short(),
        _ => mode_label(mode),
    }
}
/// The Forced Subtitles row's trailing read-out, per option in `field_options` order: a short form
/// of the picker's sentence. An unknown value reads as "Not set".
fn forced_readout(value: i64) -> &'static str {
    match value {
        0 => crate::i18n::msg::settings_audio_prefer_regular_short(),
        1 => crate::i18n::msg::settings_audio_prefer_forced_short(),
        2 => crate::i18n::msg::settings_audio_only_forced_short(),
        3 => crate::i18n::msg::settings_audio_only_regular_short(),
        _ => crate::i18n::msg::settings_audio_not_set(),
    }
}
impl Machine<InnerHost> for PreferencesPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Enter(_) => {
                // A return from a picker: its save may have confirmed a new snapshot.
                let adopted = self.txn.adopt_shared();
                if self.start_initial_load(fx) || adopted {
                    self.rebuild_keeping(); fx.invalidate(crate::ui::present::Provenance::Input);
                }
                Handled::No
            }
            ScreenEvent::Tick(t) => {
                let started = self.start_initial_load(fx);
                let stale = self.txn.stale();
                if stale { self.txn.recover_stale(&mut self.state.io, fx); }
                let adopted = self.txn.adopt_shared();
                let had_rows = self.form.table.n_rows() > 0;
                let landed = self.txn.poll(&mut self.state.io, fx) != Landed::Nothing;
                if landed || stale || started || adopted || self.state.quality != crate::route::quality()
                    || self.state.direct_play != crate::route::direct_play_mode()
                    || self.look != (crate::route::subtitle_size(), crate::route::subtitle_position())
                    || self.next_episode != crate::route::next_episode_mode()
                    || self.skip_interval != crate::route::skip_interval() {
                    self.rebuild_keeping();
                    // An empty loading table had no engine seat. Give its first landing (or
                    // Retry row) one so OK works immediately, without moving an existing
                    // cursor when an ordinary save completes or another page owns focus.
                    if !had_rows && self.form.table.n_rows() > 0 && cx.focus.current.is_none() {
                        self.focus(fx);
                    }
                    fx.invalidate(crate::ui::present::Provenance::Landing(MachineId::Session));
                }
                self.form.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                self.state.selected = self.form.key_at(self.form.table.sel.max(0) as usize).map_or(0, |k| k.0);
                Handled::Yes
            }
            ScreenEvent::Activate(key) => { self.activate(*key, fx); Handled::Yes }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Right, at_edge: true, .. }, .. }) => {
                if let Some(key) = cx.focus.current.and_then(|k| form_right_target(&self.form, k.elem)) {
                    self.activate(key, fx);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

crate::focusable_via_view!(PreferencesPage, InnerHost, view);

impl Screen<InnerHost> for PreferencesPage {
    fn name(&self) -> &'static str { self.state.kind.word() }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(crate::i18n::msg::settings_title()))
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        crate::ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
}

/// The `(focus key, destination)` of every `Nav` row a page lists for `prefs` — the structural
/// navigation test's expectation, built by the page's own form builder.
#[cfg(test)]
pub(super) fn nav_items_for_test(kind: Kind, prefs: Option<&AudioPreferences>) -> Vec<(u32, SettingsPage)> {
    let mut form: FormTable<RowId, Action, SettingsPage> = FormTable::new(BAND);
    form.set(field_form(&FieldListInputs { kind, quality: Quality::Original, direct_play: DirectPlayMode::Auto,
        prefs, busy: false, show_retry: false }), None);
    (0..form.table.n_rows() as usize).filter_map(|i| match form.binding_at(i)?.kind.clone() {
        RowKind::Nav(dest) => Some((form.key_at(i)?.0, dest)),
        _ => None,
    }).collect()
}

#[cfg(test)]
#[path = "preferences_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "preferences_text_fit_tests.rs"]
mod text_fit_tests;
