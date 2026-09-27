//! Playback defaults and Plex account preferences on the shared Settings table.
//! The confirmed account snapshot stays with this screen; workers return receipts and never
//! mutate the UI. A picker edits one field, and a failed save leaves the confirmed value intact.
use std::borrow::Cow;
use std::sync::mpsc::{self, Receiver};
use crate::plex::account::{PreferenceError, PreferenceRequest, PreferenceSnapshot, PreferenceUpdate};
use crate::route::{DirectPlayMode, Quality};
use crate::ui::decision_prompt::{DecisionPrompt, PromptStep};
use crate::ui::frame::Budget;
use crate::ui::machine::{Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId,
    Handled, InputEvent, InputKind, InstanceId, Key, LogicalState, Machine, MachineId};
use crate::ui::screen::{At, Dir, DrawFrame, Enter, FocusSource, FocusTarget, Focusable, GroupSpec,
    HitSource, Placed, RenderStrategy, Screen, ScreenEvent, Step};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::table_screen::{Header, TableScreen};
use crate::ui::route_screen::RouteLayout;
use crate::ui::{theme, Rect};
use super::family::{table_focus, InnerHost, ALERT_GROUP};
use super::registry::{AccountPreferenceReply, AppFx, PreferenceCmd, ALERT};

pub(crate) const SHAPE: &str = "PreferencesV1{kind:u8,picker:u8,selection:u32,busy:bool,status:str,quality:u8,direct_play:u8,confirm:bool,affirm:bool,values:[str]}";

const FORCE_BODY: &str = "Bypasses playback compatibility checks and always uses original quality. Playback may have no sound, display incorrectly, freeze, or crash the app. PlxNative will not automatically switch to compatible playback and may not recover gracefully.\n\nEnable this only if you understand these risks and know how to restart the app and return Direct Play to Auto.";
const FORCE_NOTE: &str = "Advanced override. Playback may fail or crash; automatic fallback is off. Return to Auto if problems occur.";
const ACCOUNT_NOTE: &str = "Saved to this Plex profile and shared with your other Plex apps. Changes may take time to affect playback; selections already made for an item still take priority.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind { Playback, AudioSubtitles }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field { Quality, DirectPlay, AudioLanguage, SubtitleMode, SubtitleLanguage, ForcedSubtitles }
impl Field {
    fn title(self) -> &'static str { match self {
        Self::Quality => "Default quality", Self::DirectPlay => "Direct Play",
        Self::AudioLanguage => "Preferred audio language", Self::SubtitleMode => "Subtitles",
        Self::SubtitleLanguage => "Subtitle language", Self::ForcedSubtitles => "Forced subtitles",
    }}
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value { Quality(Quality), DirectPlay(DirectPlayMode), Language(String), Mode(i64), Forced(i64) }
#[derive(Clone)]
enum Action { Open(Field), Pick(Value), Retry }
struct State {
    kind: Kind, picker: Option<Field>, selected: i32, busy: bool, status: String,
    quality: Quality, direct_play: DirectPlayMode, values: Vec<String>,
    confirming: bool, affirmative: bool,
}
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.u8(self.kind as u8).u8(self.picker.map_or(0, |p| p as u8 + 1))
            .u32(self.selected.max(0) as u32).bool(self.busy).str(&self.status)
            .u8(self.quality as u8).u8(self.direct_play as u8)
            .bool(self.confirming).bool(self.affirmative).u32(self.values.len() as u32);
        for v in &self.values { c.str(v); }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("preferences {:?} picker={:?} sel={} busy={} confirm={}",
            self.kind, self.picker, self.selected, self.busy, self.confirming));
    }
}
enum Pending {
    Account(Receiver<AccountPreferenceReply>),
    Local(Receiver<bool>),
}

pub(crate) struct PreferencesPage {
    entry: EntryId,
    table: TableView,
    actions: Vec<Action>,
    state: State,
    parent_row: i32,
    copy: String,
    request: Option<PreferenceRequest>,
    snapshot: Option<PreferenceSnapshot>,
    pending: Option<Pending>,
    retry: Option<PreferenceUpdate>,
    alert: DecisionPrompt,
}
impl PreferencesPage {
    pub(crate) fn new(entry: EntryId, kind: Kind) -> Self {
        let mut s = Self { entry, table: TableView::new(), actions: Vec::new(),
            state: State { kind, picker: None, selected: 0, busy: false, status: String::new(),
                quality: crate::route::quality(), direct_play: crate::route::direct_play_mode(),
                values: Vec::new(), confirming: false, affirmative: false },
            parent_row: 0, copy: String::new(), request: None, snapshot: None, pending: None, retry: None,
            alert: DecisionPrompt::new(ALERT_GROUP, ALERT, ALERT + 1, c"Cancel", c"Enable Force") };
        s.rebuild(0);
        s
    }
    fn title(&self) -> &'static str {
        self.state.picker.map_or(match self.state.kind {
            Kind::Playback => "Video & playback", Kind::AudioSubtitles => "Audio & subtitles",
        }, Field::title)
    }
    fn copy_text(&self) -> Cow<'_, str> {
        if !self.state.status.is_empty() {
            return if self.state.kind == Kind::Playback && self.state.direct_play == DirectPlayMode::Forced {
                Cow::Owned(format!("{}\n\n{FORCE_NOTE}", self.state.status))
            } else { Cow::Borrowed(&self.state.status) };
        }
        match self.state.kind {
            Kind::AudioSubtitles => Cow::Borrowed(ACCOUNT_NOTE),
            Kind::Playback if self.state.direct_play == DirectPlayMode::Forced => Cow::Borrowed(FORCE_NOTE),
            Kind::Playback => Cow::Borrowed("Playback defaults for this television. Quality is also available in the player's More menu. Changes here apply to the next playback."),
        }
    }
    fn view(&self) -> TableScreen<'_> {
        let crumb = if self.state.picker.is_some() { match self.state.kind {
            Kind::Playback => "Video & playback", Kind::AudioSubtitles => "Audio & subtitles",
        }} else { "Settings" };
        TableScreen::new(Header::new(RouteLayout::screen(), Some(crumb), self.title(), &self.copy),
            &self.table, GroupId(0), self.entry)
    }
    fn focus(&self, fx: &mut Effects<'_, InnerHost>, group: GroupId) {
        fx.push(Fx::Deliver(MachineId::Instance(InstanceId(0)),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: if group == GroupId(0) {
                FocusTarget::Elem(FocusKey { entry: self.entry, elem: self.table.sel.max(0) as u32 })
            } else { FocusTarget::ContainerGroup(group) } }))));
    }
    fn load(&mut self, fx: &mut Effects<'_, InnerHost>) {
        self.request = None;
        self.retry = None;
        self.start_account(None, fx);
    }
    fn start_account(&mut self, update: Option<PreferenceUpdate>, fx: &mut Effects<'_, InnerHost>) {
        if self.pending.is_some() { return; }
        self.retry = update.clone();
        let (reply, rx) = mpsc::channel();
        let command = match (update, self.request.clone(), self.snapshot.clone()) {
            (Some(update), Some(request), Some(base)) => PreferenceCmd::Save { request, base, update, reply },
            _ => PreferenceCmd::Load { reply },
        };
        self.state.status = if matches!(&command, PreferenceCmd::Save { .. }) {
            "Saving to your Plex account…"
        } else { "Loading your Plex account preferences…" }.into();
        self.pending = Some(Pending::Account(rx)); self.state.busy = true;
        fx.push(Fx::App(AppFx::Preferences(command)));
    }
    fn save_local(&mut self, value: Value, fx: &mut Effects<'_, InnerHost>) {
        let (reply, rx) = mpsc::channel();
        let command = match value {
            Value::Quality(quality) => PreferenceCmd::Quality { quality, reply },
            Value::DirectPlay(mode) => PreferenceCmd::DirectPlay { mode, reply },
            _ => return,
        };
        self.pending = Some(Pending::Local(rx)); self.state.busy = true;
        self.state.status = "Saving playback preference…".into();
        fx.push(Fx::App(AppFx::Preferences(command)));
    }
    fn start_initial_load(&mut self, fx: &mut Effects<'_, InnerHost>) -> bool {
        if self.state.kind == Kind::AudioSubtitles && self.request.is_none()
            && self.snapshot.is_none() && self.pending.is_none() && self.state.status.is_empty() {
            self.load(fx);
            true
        } else { false }
    }
    fn current_value(&self, field: Field) -> Value {
        let p = self.snapshot.as_ref().map(|s| &s.preferences);
        match field {
            Field::Quality => Value::Quality(self.state.quality),
            Field::DirectPlay => Value::DirectPlay(self.state.direct_play),
            Field::AudioLanguage => Value::Language(p.and_then(|p| p.stated_language.clone()).unwrap_or_default()),
            Field::SubtitleLanguage => Value::Language(p.and_then(|p| p.subtitle_language.clone()).unwrap_or_default()),
            Field::SubtitleMode => Value::Mode(p.map_or(0, |p| p.subtitle_mode)),
            Field::ForcedSubtitles => Value::Forced(p.map_or(0, |p| p.subtitle_forced)),
        }
    }
    fn options(&self, field: Field) -> Vec<(String, Value)> {
        match field {
            Field::Quality => crate::route::available_quality_ladder().iter()
                .map(|q| (q.label().into(), Value::Quality(*q))).collect(),
            Field::DirectPlay => [DirectPlayMode::Auto, DirectPlayMode::Forced, DirectPlayMode::Disabled]
                .into_iter().map(|m| (mode_label(m).into(), Value::DirectPlay(m))).collect(),
            Field::SubtitleMode => [("Off (manual selection)", 0), ("When audio isn't in my language", 1), ("Always", 2)]
                .into_iter().map(|(label, mode)| (label.into(), Value::Mode(mode))).collect(),
            Field::ForcedSubtitles => ["Prefer non-forced subtitles", "Prefer forced subtitles", "Only forced subtitles", "Only non-forced subtitles"]
                .into_iter().enumerate().map(|(i, s)| (s.into(), Value::Forced(i as i64))).collect(),
            Field::AudioLanguage | Field::SubtitleLanguage => {
                let mut result = vec![(if field == Field::AudioLanguage { "Original" } else { "No preference" }.into(), Value::Language(String::new()))];
                result.extend(crate::plex::languages::LANGUAGES.iter()
                    .map(|l| (l.name.to_string(), Value::Language(l.code.to_string()))));
                let current = self.current_value(field);
                if !result.iter().any(|(_, v)| *v == current) {
                    if let Value::Language(code) = current { result.push((code.clone(), Value::Language(code))); }
                }
                result
            }
        }
    }
    fn value_label(&self, field: Field) -> String {
        let current = self.current_value(field);
        self.options(field).into_iter().find(|(_, v)| *v == current).map_or_else(|| "Not set".into(), |(s, _)| s)
    }
    fn rebuild(&mut self, selected: i32) {
        self.state.quality = crate::route::quality();
        self.state.direct_play = crate::route::direct_play_mode();
        self.copy = self.copy_text().into_owned();
        self.state.confirming = self.alert.is_open(); self.state.affirmative = self.alert.choice();
        self.state.values.clear(); self.actions.clear();
        let mut section = Section::new("");
        if let Some(field) = self.state.picker {
            let current = self.current_value(field);
            for (label, value) in self.options(field) {
                section = section.row(Row::new(&label).checked(value == current).dim(self.state.busy));
                self.state.values.push(label); self.actions.push(Action::Pick(value));
            }
        } else {
            let fields: &[Field] = match self.state.kind {
                Kind::Playback => &[Field::Quality, Field::DirectPlay],
                Kind::AudioSubtitles if self.snapshot.is_some() => &[Field::AudioLanguage, Field::SubtitleMode, Field::SubtitleLanguage, Field::ForcedSubtitles],
                _ => &[],
            };
            for &field in fields {
                let value = self.value_label(field);
                let mut row = Row::new(field.title()).value(&value).chevron(true).dim(self.state.busy);
                if field == Field::Quality && self.state.direct_play == DirectPlayMode::Forced {
                    row = row.detail("Overridden by Force Direct Play: Original quality.");
                }
                if field == Field::AudioLanguage && self.snapshot.as_ref().is_some_and(|s| s.preferences.auto_select_audio == Some(false)) {
                    row = row.detail("Automatic selection is off in Plex. Choosing a language enables it.");
                }
                section = section.row(row); self.state.values.push(value); self.actions.push(Action::Open(field));
            }
            if self.state.kind == Kind::AudioSubtitles && !self.state.busy && !self.state.status.is_empty() {
                section = section.row(Row::new("Retry").detail("Try the account request again.")); self.actions.push(Action::Retry);
            }
        }
        self.table.compact = false; self.table.header_ink = theme::TEXT_READING;
        self.table.set_sections(vec![section], selected, false); self.table.list_focused = true;
        self.state.selected = self.table.sel;
    }
    fn close_picker(&mut self, fx: &mut Effects<'_, InnerHost>) {
        self.state.picker = None; self.rebuild(self.parent_row); self.focus(fx, GroupId(0));
    }
    fn activate(&mut self, row: usize, fx: &mut Effects<'_, InnerHost>) {
        if self.state.busy { return; }
        let Some(action) = self.actions.get(row).cloned() else { return; };
        match action {
            Action::Open(field) => {
                self.parent_row = row as i32; self.state.picker = Some(field);
                let current = self.current_value(field);
                let selected = self.options(field).iter().position(|(_, v)| *v == current).unwrap_or(0);
                self.rebuild(selected as i32); self.focus(fx, GroupId(0));
            }
            Action::Pick(value) => {
                if value == Value::DirectPlay(DirectPlayMode::Forced) && self.state.direct_play != DirectPlayMode::Forced {
                    self.alert.open(c"Force Direct Play?", FORCE_BODY);
                    self.state.confirming = true; self.state.affirmative = false; self.focus(fx, ALERT_GROUP);
                    return;
                }
                self.commit(value, fx); self.close_picker(fx);
            }
            Action::Retry => {
                if let Some(update) = self.retry.clone() { self.start_account(Some(update), fx); }
                else { self.load(fx); }
                self.rebuild(self.table.sel);
            }
        }
        fx.invalidate(crate::ui::present::Provenance::Input);
    }
    fn commit(&mut self, value: Value, fx: &mut Effects<'_, InnerHost>) {
        match value {
            Value::Quality(_) | Value::DirectPlay(_) => self.save_local(value, fx),
            value => {
                let mut update = PreferenceUpdate::default();
                match (self.state.picker, value) {
                    (Some(Field::AudioLanguage), Value::Language(v)) => {
                        update.audio_language = Some(v); update.auto_select_audio = Some(true);
                    }
                    (Some(Field::SubtitleLanguage), Value::Language(v)) => update.subtitle_language = Some(v),
                    (_, Value::Mode(v)) => {
                        update.subtitle_mode = Some(v);
                        if v > 0 { update.auto_select_audio = Some(true); }
                    },
                    (_, Value::Forced(v)) => update.subtitle_forced = Some(v),
                    _ => return,
                }
                self.start_account(Some(update), fx);
            }
        }
    }
    fn poll(&mut self, fx: &mut Effects<'_, InnerHost>) -> bool {
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
        let Some(receipt) = receipt else { return false; };
        self.pending = None; self.state.busy = false;
        match receipt {
            Receipt::Account(reply) => {
                if reply.request.as_ref().is_some_and(|request| !request.is_current()) {
                    self.snapshot = None; self.state.picker = None; self.load(fx);
                } else if let Some(request) = reply.request {
                    self.request = Some(request);
                    match reply.outcome {
                        Ok(snapshot) => { self.snapshot = Some(snapshot); self.retry = None; self.state.status.clear(); }
                        Err(error) => {
                            if error == PreferenceError::Stale { self.retry = None; }
                            self.state.status = format!("{} Select Retry.", error.message());
                        }
                    }
                } else {
                    self.request = None; self.snapshot = None; self.retry = None;
                    self.state.status = "Sign in to this Plex profile again to edit its account preferences.".into();
                }
            }
            Receipt::Local(true) => self.state.status.clear(),
            Receipt::Local(false) | Receipt::Failed => self.state.status = "Could not save or load this preference. Please try again.".into(),
        }
        true
    }
}
fn mode_label(mode: DirectPlayMode) -> &'static str {
    match mode { DirectPlayMode::Auto => "Auto", DirectPlayMode::Forced => "Force Direct Play (advanced)", DirectPlayMode::Disabled => "Disabled" }
}
impl Machine<InnerHost> for PreferencesPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match self.alert.step(ev, cx) {
            PromptStep::Pass => (),
            PromptStep::Done(handled) => { self.state.affirmative = self.alert.choice(); return handled; }
            PromptStep::Answer(yes) => {
                if yes { self.save_local(Value::DirectPlay(DirectPlayMode::Forced), fx); }
                self.state.confirming = false;
                if yes { self.close_picker(fx); } else { self.focus(fx, GroupId(0)); }
                fx.invalidate(crate::ui::present::Provenance::Input); return Handled::Yes;
            }
        }
        match ev {
            ScreenEvent::Enter(_) => {
                if self.start_initial_load(fx) {
                    self.rebuild(self.table.sel); fx.invalidate(crate::ui::present::Provenance::Input);
                }
                Handled::No
            }
            ScreenEvent::Tick(t) => {
                self.alert.update(t.dt());
                let started = self.start_initial_load(fx);
                let stale = self.request.as_ref().is_some_and(|r| !r.is_current());
                if stale {
                    self.snapshot = None; self.pending = None; self.state.busy = false;
                    self.state.picker = None; self.load(fx);
                }
                let had_rows = self.table.n_rows() > 0;
                if self.poll(fx) || stale || started || self.state.quality != crate::route::quality()
                    || self.state.direct_play != crate::route::direct_play_mode() {
                    self.rebuild(self.table.sel);
                    // An empty loading table had no engine seat. Give its first landing (or
                    // Retry row) one so OK works immediately, without moving an existing
                    // cursor when an ordinary save completes or another page owns focus.
                    if !had_rows && self.table.n_rows() > 0 && cx.focus.current.is_none() {
                        self.focus(fx, GroupId(0));
                    }
                    fx.invalidate(crate::ui::present::Provenance::Landing(MachineId::Session));
                }
                self.table.update(t.dt(), RouteLayout::screen().sectioned_table().h); Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                table_focus(&mut self.table, to.elem); self.state.selected = self.table.sel; Handled::Yes
            }
            ScreenEvent::Activate(elem) => { self.activate(*elem as usize, fx); Handled::Yes }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. }, .. }) if self.state.picker.is_some() => {
                self.close_picker(fx); Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Right, at_edge: true, .. }, .. }) if self.state.picker.is_none() => {
                if let Some(key) = cx.focus.current { self.activate(key.elem as usize, fx); } Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl Focusable<InnerHost> for PreferencesPage {
    fn groups(&self, cx: &Cx<'_, InnerHost>, out: &mut Vec<GroupSpec>) {
        if !self.alert.groups(out) {
            Focusable::<InnerHost>::groups(&self.view(), cx, out)
        }
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, InnerHost>) -> Option<GroupId> {
        match self.alert.group_of(*key) {
            Some(answer) => answer,
            None => Focusable::<InnerHost>::group_of(&self.view(), key, cx),
        }
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, InnerHost>) -> Step<u32> {
        match self.alert.neighbour(key, dir) {
            Some(step) => step,
            None => Focusable::<InnerHost>::neighbour(&self.view(), key, dir, cx),
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, InnerHost>, at: At) -> Option<Placed> {
        match self.alert.place(*key) {
            Some(placed) => placed,
            None => Focusable::<InnerHost>::place(&self.view(), key, cx, at),
        }
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        if let Some(key) = self.alert.reconcile(want) {
            return key;
        }
        if self.alert.owns(want.elem) {
            return FocusKey { entry: self.entry, elem: self.table.sel.max(0) as u32 };
        }
        Focusable::<InnerHost>::reconcile(&self.view(), want, cx)
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        match self.alert.seat(g, self.entry) {
            Some(key) => key,
            None => Focusable::<InnerHost>::seat(&self.view(), g, from, cx),
        }
    }
}

impl Screen<InnerHost> for PreferencesPage {
    fn name(&self) -> &'static str {
        match self.state.kind { Kind::Playback => "settings-playback", Kind::AudioSubtitles => "settings-audio" }
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed("Settings"))
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        let mut v = self.view();
        crate::ui::screen::Part::<InnerHost>::draw(&mut v, f, Rect::FULL);
        self.alert.draw(f, self.entry);
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


#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::machine::{FocusRead, InputOwner, PressRead, Tick};
    use crate::ui::present::Present;
    use crate::ui::fixture::FixtureMeasure;

    fn with_fx(f: impl FnOnce(&mut Effects<'_, InnerHost>)) {
        let mut out = Vec::new(); let mut present = Present::new();
        f(&mut Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present));
    }
    fn context(focus: u32) -> Cx<'static, InnerHost> {
        static MEASURE: FixtureMeasure = FixtureMeasure;
        Cx { views: crate::stores::browse::DirectoryView::empty_for_test(), tick: Tick::default(),
            measure: &MEASURE, press: PressRead::default(),
            focus: FocusRead { current: Some(FocusKey { entry: EntryId(0), elem: focus }), ..Default::default() },
            owner: InputOwner::Entry(EntryId(0)) }
    }
    #[test]
    fn force_requires_acknowledgement_and_cancel_never_saves() {
        let _serial = crate::testlock::serial();
        let _session = crate::plex::session::TempSession::new("force-confirm-cancel");
        crate::route::restore_direct_play_mode(DirectPlayMode::Auto);
        let mut page = PreferencesPage::new(EntryId(0), Kind::Playback);
        with_fx(|fx| page.activate(1, fx));
        assert_eq!(page.state.picker, Some(Field::DirectPlay));
        with_fx(|fx| page.activate(1, fx));
        assert!(page.alert.is_open());
        assert!(!page.alert.choice(), "Cancel is the default answer");
        assert!(page.pending.is_none(), "opening the warning must not persist Force");
        assert_eq!(crate::route::direct_play_mode(), DirectPlayMode::Auto);
        let cx = context(ALERT);
        let cancel = ScreenEvent::Input(InputEvent { at: Tick::default(), source: crate::ui::machine::Source::RemoteFifo,
            kind: InputKind::Key { key: Key::Back, edge: Edge::Down, sym: 0, wcode: 0, at_edge: false } });
        with_fx(|fx| { page.step(&cancel, &cx, fx); });
        assert!(!page.alert.is_open());
        assert!(page.pending.is_none());
        assert_eq!(crate::route::direct_play_mode(), DirectPlayMode::Auto);
    }
    #[test]
    fn account_constructor_is_inert_and_first_enter_emits_one_load_effect() {
        let _serial = crate::testlock::serial();
        let mut page = PreferencesPage::new(EntryId(0), Kind::AudioSubtitles);
        assert!(page.pending.is_none());
        assert!(page.request.is_none());
        assert!(page.state.status.is_empty());
        let mut emitted = Vec::new(); let mut present = Present::new();
        let mut fx = Effects::new(&mut emitted, MachineId::Instance(InstanceId(0)), &mut present);
        let cx = context(0);
        page.step(&ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::ContainerGroup(GroupId(0)) }), &cx, &mut fx);
        page.step(&ScreenEvent::Tick(Tick::default()), &cx, &mut fx);
        assert!(page.state.busy);
        assert!(page.request.is_none(), "the screen never captures live credentials");
        assert_eq!(emitted.iter().filter(|event| matches!(&event.fx,
            Fx::App(AppFx::Preferences(PreferenceCmd::Load { .. })))).count(), 1);
    }

    #[test]
    fn confirming_force_emits_a_preference_effect_without_executing_it() {
        let _serial = crate::testlock::serial();
        let previous = crate::route::direct_play_mode();
        crate::route::restore_direct_play_mode(DirectPlayMode::Auto);
        let mut page = PreferencesPage::new(EntryId(0), Kind::Playback);
        with_fx(|fx| page.activate(1, fx));
        with_fx(|fx| page.activate(1, fx));
        let mut emitted = Vec::new(); let mut present = Present::new();
        let mut fx = Effects::new(&mut emitted, MachineId::Instance(InstanceId(0)), &mut present);
        page.step(&ScreenEvent::PressCommit(crate::ui::machine::PressId(1)), &context(ALERT + 1), &mut fx);
        assert!(page.state.busy);
        assert!(emitted.iter().any(|event| matches!(&event.fx,
            Fx::App(AppFx::Preferences(PreferenceCmd::DirectPlay { mode: DirectPlayMode::Forced, .. })))));
        assert_eq!(crate::route::direct_play_mode(), DirectPlayMode::Auto,
            "only an admitted app executor can persist and activate Force");
        crate::route::restore_direct_play_mode(previous);
    }

    #[test]
    fn both_language_pickers_offer_the_full_catalog_and_an_empty_preference() {
        let _serial = crate::testlock::serial();
        let page = PreferencesPage::new(EntryId(0), Kind::Playback);
        for field in [Field::AudioLanguage, Field::SubtitleLanguage] {
            let options = page.options(field);
            assert_eq!(options.len(), crate::plex::languages::LANGUAGES.len() + 1);
            assert_eq!(options[0].1, Value::Language(String::new()));
            assert!(options.len() > 100);
        }
    }
    #[test]
    fn picker_back_restores_parent_row_without_saving() {
        let _serial = crate::testlock::serial();
        let mut page = PreferencesPage::new(EntryId(0), Kind::Playback);
        with_fx(|fx| page.activate(1, fx));
        with_fx(|fx| page.close_picker(fx));
        assert_eq!(page.state.picker, None);
        assert_eq!(page.table.sel, 1);
        assert!(page.pending.is_none());
    }
}
