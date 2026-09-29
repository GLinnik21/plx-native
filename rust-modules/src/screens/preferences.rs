//! Playback defaults and Plex account preferences on the shared Settings table.
//! The confirmed account snapshot stays with this screen; workers return receipts and never
//! mutate the UI. A picker edits one field, and a failed save leaves the confirmed value intact.
//! A picker is this page's own submenu over the confirmed snapshot it edits, so it is a second
//! table rather than a page of the surface's stack — but it moves on the family's one push spring
//! (`route_screen::RoutePush`, the surface's own), sliding in and out exactly like a pushed page.
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
use crate::ui::route_screen::{RouteLayout, RoutePush};
use crate::ui::{theme, Painter, Rect};
use super::family::{table_focus, InnerHost, ALERT_GROUP};
use super::registry::{AccountPreferenceReply, AppFx, PreferenceCmd, ALERT};

pub(crate) const SHAPE: &str = "PreferencesV2{kind:u8,picker:u8,selection:u32,busy:bool,status:str,quality:u8,direct_play:u8,confirm:bool,affirm:bool,alert_scroll:u32,values:[str]}";


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind { Playback, AudioSubtitles }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field { Quality, DirectPlay, AudioLanguage, SubtitleMode, SubtitleLanguage, ForcedSubtitles }
impl Field {
    fn title(self) -> &'static str { match self {
        Self::Quality => crate::i18n::msg::settings_playback_quality(), Self::DirectPlay => crate::i18n::msg::settings_playback_direct_play(),
        Self::AudioLanguage => crate::i18n::msg::settings_audio_language(), Self::SubtitleMode => crate::i18n::msg::settings_audio_subtitles(),
        Self::SubtitleLanguage => crate::i18n::msg::settings_audio_subtitle_language(), Self::ForcedSubtitles => crate::i18n::msg::settings_audio_forced_subtitles(),
    }}
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value { Quality(Quality), DirectPlay(DirectPlayMode), Language(String), Mode(i64), Forced(i64) }
#[derive(Clone)]
enum Action { Open(Field), Pick(Value), Retry }
struct State {
    kind: Kind, picker: Option<Field>, selected: i32, busy: bool, status: String,
    quality: Quality, direct_play: DirectPlayMode, values: Vec<String>,
    confirming: bool, affirmative: bool, alert_scroll: u32,
}
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.u8(self.kind as u8).u8(self.picker.map_or(0, |p| p as u8 + 1))
            .u32(self.selected.max(0) as u32).bool(self.busy).str(&self.status)
            .u8(self.quality as u8).u8(self.direct_play as u8)
            .bool(self.confirming).bool(self.affirmative).u32(self.alert_scroll).u32(self.values.len() as u32);
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
    /// The page's own list of fields — the parent level of every picker.
    table: TableView,
    /// The open (or leaving) picker's options.
    picker_table: TableView,
    /// The submenu push: open while a picker is, run back on close.
    submenu: RoutePush,
    /// The picker sliding back out, still drawn in the child role until `submenu` rests closed.
    leaving: Option<Field>,
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
        let mut s = Self { entry, table: TableView::new(), picker_table: TableView::new(),
            submenu: RoutePush::new(), leaving: None, actions: Vec::new(),
            state: State { kind, picker: None, selected: 0, busy: false, status: String::new(),
                quality: crate::route::quality(), direct_play: crate::route::direct_play_mode(),
                values: Vec::new(), confirming: false, affirmative: false, alert_scroll: 0 },
            parent_row: 0, copy: String::new(), request: None, snapshot: None, pending: None, retry: None,
            alert: DecisionPrompt::new(ALERT_GROUP, ALERT, ALERT + 1, crate::i18n::msg::settings_cancel_c(), crate::i18n::msg::settings_playback_enable_force_c()) };
        s.rebuild(0);
        s
    }
    fn kind_title(&self) -> &'static str {
        match self.state.kind {
            Kind::Playback => crate::i18n::msg::settings_playback_title(), Kind::AudioSubtitles => crate::i18n::msg::settings_audio_title(),
        }
    }
    /// The table focus and activation act on: the open picker's, else the field list.
    fn active(&self) -> &TableView {
        if self.state.picker.is_some() { &self.picker_table } else { &self.table }
    }
    fn active_mut(&mut self) -> &mut TableView {
        if self.state.picker.is_some() { &mut self.picker_table } else { &mut self.table }
    }
    fn copy_text(&self) -> Cow<'_, str> {
        if !self.state.status.is_empty() {
            return if self.state.kind == Kind::Playback && self.state.direct_play == DirectPlayMode::Forced {
                Cow::Owned(format!("{}\n\n{}", self.state.status, crate::i18n::msg::settings_playback_force_note()))
            } else { Cow::Borrowed(&self.state.status) };
        }
        match self.state.kind {
            Kind::AudioSubtitles => Cow::Borrowed(crate::i18n::msg::settings_audio_account_note()),
            Kind::Playback if self.state.direct_play == DirectPlayMode::Forced => Cow::Borrowed(crate::i18n::msg::settings_playback_force_note()),
            Kind::Playback => Cow::Borrowed(crate::i18n::msg::settings_playback_copy()),
        }
    }
    fn view(&self) -> TableScreen<'_> {
        self.view_at(self.state.picker)
    }
    /// One level of the page: the field list (`None`) or a picker, each with its own crumb/title.
    fn view_at(&self, level: Option<Field>) -> TableScreen<'_> {
        let (crumb, title, table) = match level {
            Some(field) => (self.kind_title(), field.title(), &self.picker_table),
            None => (crate::i18n::msg::settings_title(), self.kind_title(), &self.table),
        };
        TableScreen::new(Header::new(RouteLayout::screen(), Some(crumb), title, &self.copy),
            table, GroupId(0), self.entry)
    }
    /// Draw one level through `p` and hand back the focus stops it registered.
    fn draw_level(&self, f: &DrawFrame<'_, '_, InnerHost>, level: Option<Field>, p: Painter) -> Vec<crate::ui::screen::Stop<u32>> {
        let mut inner = DrawFrame::with_navigation(f.cx, p, f.navigation());
        let mut v = self.view_at(level);
        crate::ui::screen::Part::<InnerHost>::draw(&mut v, &mut inner, Rect::FULL);
        inner.into_stops()
    }
    fn focus(&self, fx: &mut Effects<'_, InnerHost>, group: GroupId) {
        fx.push(Fx::Deliver(MachineId::Instance(InstanceId(0)),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: if group == GroupId(0) {
                FocusTarget::Elem(FocusKey { entry: self.entry, elem: self.active().sel.max(0) as u32 })
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
            crate::i18n::msg::settings_audio_saving()
        } else { crate::i18n::msg::settings_audio_loading() }.into();
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
        self.state.status = crate::i18n::msg::settings_playback_saving().into();
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
            Field::SubtitleMode => [(crate::i18n::msg::settings_audio_manual(), 0), (crate::i18n::msg::settings_audio_foreign(), 1), (crate::i18n::msg::settings_audio_always(), 2)]
                .into_iter().map(|(label, mode)| (label.into(), Value::Mode(mode))).collect(),
            Field::ForcedSubtitles => [crate::i18n::msg::settings_audio_prefer_regular(), crate::i18n::msg::settings_audio_prefer_forced(), crate::i18n::msg::settings_audio_only_forced(), crate::i18n::msg::settings_audio_only_regular()]
                .into_iter().enumerate().map(|(i, s)| (s.into(), Value::Forced(i as i64))).collect(),
            Field::AudioLanguage | Field::SubtitleLanguage => {
                let mut result = vec![(if field == Field::AudioLanguage { crate::i18n::msg::settings_audio_original() } else { crate::i18n::msg::settings_audio_no_preference() }.into(), Value::Language(String::new()))];
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
        self.options(field).into_iter().find(|(_, v)| *v == current).map_or_else(|| crate::i18n::msg::settings_audio_not_set().into(), |(s, _)| s)
    }
    fn rebuild(&mut self, selected: i32) {
        self.state.quality = crate::route::quality();
        self.state.direct_play = crate::route::direct_play_mode();
        self.copy = self.copy_text().into_owned();
        self.state.confirming = self.alert.is_open(); self.state.affirmative = self.alert.choice();
        self.state.alert_scroll = self.alert.scroll_target_bits();
        self.state.values.clear(); self.actions.clear();
        // The field list is rebuilt at every level: it is the parent a picker slides over.
        let mut section = Section::new("");
        let fields: &[Field] = match self.state.kind {
            Kind::Playback => &[Field::Quality, Field::DirectPlay],
            Kind::AudioSubtitles if self.snapshot.is_some() => &[Field::AudioLanguage, Field::SubtitleMode, Field::SubtitleLanguage, Field::ForcedSubtitles],
            _ => &[],
        };
        let mut list_values = Vec::new(); let mut list_actions = Vec::new();
        for &field in fields {
            let value = self.value_label(field);
            let mut row = Row::new(field.title()).value(&value).chevron(true).dim(self.state.busy);
            if field == Field::Quality && self.state.direct_play == DirectPlayMode::Forced {
                row = row.detail(crate::i18n::msg::settings_playback_overridden());
            }
            if field == Field::AudioLanguage && self.snapshot.as_ref().is_some_and(|s| s.preferences.auto_select_audio == Some(false)) {
                row = row.detail(crate::i18n::msg::settings_audio_selection_off());
            }
            section = section.row(row); list_values.push(value); list_actions.push(Action::Open(field));
        }
        if self.state.kind == Kind::AudioSubtitles && !self.state.busy && !self.state.status.is_empty() {
            section = section.row(Row::new(crate::i18n::msg::settings_audio_retry()).detail(crate::i18n::msg::settings_audio_retry_detail())); list_actions.push(Action::Retry);
        }
        let list_sel = if self.state.picker.is_some() { self.parent_row } else { selected };
        for table in [&mut self.table, &mut self.picker_table] {
            table.compact = false; table.header_ink = theme::TEXT_READING; table.list_focused = true;
        }
        self.table.set_sections(vec![section], list_sel, false);
        if let Some(field) = self.state.picker {
            let current = self.current_value(field);
            let mut section = Section::new("");
            for (label, value) in self.options(field) {
                section = section.row(Row::new(&label).checked(value == current).dim(self.state.busy));
                self.state.values.push(label); self.actions.push(Action::Pick(value));
            }
            self.picker_table.set_sections(vec![section], selected, false);
        } else {
            self.state.values = list_values; self.actions = list_actions;
        }
        self.state.selected = self.active().sel;
    }
    fn close_picker(&mut self, fx: &mut Effects<'_, InnerHost>) {
        self.leaving = self.state.picker.take().or(self.leaving);
        self.rebuild(self.parent_row); self.focus(fx, GroupId(0));
    }
    fn activate(&mut self, row: usize, fx: &mut Effects<'_, InnerHost>) {
        if self.state.busy { return; }
        let Some(action) = self.actions.get(row).cloned() else { return; };
        match action {
            Action::Open(field) => {
                self.parent_row = row as i32; self.state.picker = Some(field); self.leaving = None;
                let current = self.current_value(field);
                let selected = self.options(field).iter().position(|(_, v)| *v == current).unwrap_or(0);
                self.rebuild(selected as i32); self.focus(fx, GroupId(0));
            }
            Action::Pick(value) => {
                if value == Value::DirectPlay(DirectPlayMode::Forced) && self.state.direct_play != DirectPlayMode::Forced {
                    self.alert.open(crate::i18n::msg::settings_playback_force_question_c(), crate::i18n::msg::settings_playback_force_body());
                    self.state.confirming = true; self.state.affirmative = false;
                    self.state.alert_scroll = self.alert.scroll_target_bits(); self.focus(fx, ALERT_GROUP);
                    return;
                }
                self.commit(value, fx); self.close_picker(fx);
            }
            Action::Retry => {
                if let Some(update) = self.retry.clone() { self.start_account(Some(update), fx); }
                else { self.load(fx); }
                self.rebuild(self.active().sel);
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
                            self.state.status = crate::i18n::msg::settings_audio_error_retry(error.message());
                        }
                    }
                } else {
                    self.request = None; self.snapshot = None; self.retry = None;
                    self.state.status = crate::i18n::msg::settings_audio_sign_in_again().into();
                }
            }
            Receipt::Local(true) => self.state.status.clear(),
            Receipt::Local(false) | Receipt::Failed => self.state.status = crate::i18n::msg::settings_playback_save_failed().into(),
        }
        true
    }
}
fn mode_label(mode: DirectPlayMode) -> &'static str {
    match mode { DirectPlayMode::Auto => crate::i18n::msg::settings_playback_auto(), DirectPlayMode::Forced => crate::i18n::msg::settings_playback_forced(), DirectPlayMode::Disabled => crate::i18n::msg::settings_playback_disabled() }
}
impl Machine<InnerHost> for PreferencesPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match self.alert.step(ev, cx) {
            PromptStep::Pass => (),
            PromptStep::Done(handled) => {
                self.state.affirmative = self.alert.choice();
                self.state.alert_scroll = self.alert.scroll_target_bits();
                return handled;
            }
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
                    self.rebuild(self.active().sel); fx.invalidate(crate::ui::present::Provenance::Input);
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
                let had_rows = self.active().n_rows() > 0;
                if self.poll(fx) || stale || started || self.state.quality != crate::route::quality()
                    || self.state.direct_play != crate::route::direct_play_mode() {
                    self.rebuild(self.active().sel);
                    // An empty loading table had no engine seat. Give its first landing (or
                    // Retry row) one so OK works immediately, without moving an existing
                    // cursor when an ordinary save completes or another page owns focus.
                    if !had_rows && self.active().n_rows() > 0 && cx.focus.current.is_none() {
                        self.focus(fx, GroupId(0));
                    }
                    fx.invalidate(crate::ui::present::Provenance::Landing(MachineId::Session));
                }
                let h = RouteLayout::screen().sectioned_table().h;
                self.table.update(t.dt(), h); self.picker_table.update(t.dt(), h);
                let open = self.state.picker.is_some();
                self.submenu.tick(open, *t, &mut fx.present());
                if !open && self.submenu.resting(false) { self.leaving = None; }
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                table_focus(self.active_mut(), to.elem); self.state.selected = self.active().sel; Handled::Yes
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
        if !self.alert.groups(out, cx.measure) {
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
        match self.alert.place(*key, cx.measure) {
            Some(placed) => placed,
            None => Focusable::<InnerHost>::place(&self.view(), key, cx, at),
        }
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        if let Some(key) = self.alert.reconcile(want) {
            return key;
        }
        if self.alert.owns(want.elem) {
            return FocusKey { entry: self.entry, elem: self.active().sel.max(0) as u32 };
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
        Some(Cow::Borrowed(crate::i18n::msg::settings_title()))
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        let open = self.state.picker.is_some();
        if self.submenu.resting(open) {
            let mut v = self.view();
            crate::ui::screen::Part::<InnerHost>::draw(&mut v, f, Rect::FULL);
        } else {
            // Mid-push: the field list in the parent role, the picker (or the one leaving) in
            // the child role; only the level focus acts on contributes stops.
            let (parent_p, child_p) = (self.submenu.parent(f.painter), self.submenu.child(f.painter));
            let parent = self.draw_level(f, None, parent_p);
            let child = self.state.picker.or(self.leaving)
                .map(|field| self.draw_level(f, Some(field), child_p)).unwrap_or_default();
            for s in if open { child } else { parent } { f.stop(Painter::root(), s); }
        }
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
    /// Owner report: Direct Play / Default Quality (and every Audio & Subtitles picker) switched
    /// to the next screen instantly. A picker is this page's own submenu, so it must run the
    /// family's push spring — in on open, back out on close with the picker still drawn.
    #[test]
    fn a_picker_slides_in_and_out_on_the_family_push_spring() {
        let _serial = crate::testlock::serial();
        let _session = crate::plex::session::TempSession::new("picker-push-spring");
        let mut page = PreferencesPage::new(EntryId(0), Kind::Playback);
        assert!(page.submenu.resting(false), "no picker: the list is at rest");
        with_fx(|fx| page.activate(0, fx));
        assert_eq!(page.state.picker, Some(Field::Quality));
        assert!(page.submenu.amount() < 0.01 && !page.submenu.resting(true),
            "opening a picker starts the push rather than cutting to it");
        let t = Tick { ms: 16, dt_us: 16_000 };
        for _ in 0..120 { with_fx(|fx| { page.step(&ScreenEvent::Tick(t), &context(0), fx); }); }
        assert!(page.submenu.resting(true), "the push settles open");
        with_fx(|fx| page.close_picker(fx));
        assert_eq!(page.state.picker, None);
        assert!(page.submenu.amount() > 0.99 && !page.submenu.resting(false),
            "closing runs the same spring back");
        assert_eq!(page.leaving, Some(Field::Quality), "the picker keeps drawing while it leaves");
        for _ in 0..120 { with_fx(|fx| { page.step(&ScreenEvent::Tick(t), &context(0), fx); }); }
        assert!(page.submenu.resting(false));
        assert_eq!(page.leaving, None, "…and is released once the spring settles");
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
