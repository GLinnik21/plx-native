//! **A preference's choice list, as a page of the Settings stack** (`SettingsPage::Picker`). It
//! renders exactly what the field list's in-page submenu used to: crumb = the parent page's title,
//! title = the field's, the field's own explanation (see [`copy_text`]), one checked `Choice` row per option, opened on
//! the checked one.
//!
//! **Keys and ids.** A row's id is its option's [`Value`]; its focus key is the option's POSITION in
//! [`field_options`]' order (deterministic from the inputs, so a replay addresses the same row). The
//! Retry row of a failed account write takes [`RETRY_KEY`], a number no option list reaches.
//!
//! **The transaction.** The page owns it end to end: it adopts the parent's confirmed snapshot (or
//! loads one when there is none), asks the Direct Play *Forced* question before persisting it,
//! submits through the same `AppFx::Preferences` paths, and pops on a DURABLE receipt. Choosing the
//! checked option pops without a write (compared against the canonical current value, so a stored
//! deprecated code is never rewritten by a pick that changed nothing) — except Size and Position,
//! which publish their live value before the write lands: there the checked row must also match the
//! STORED value, so OK on it after a failed write retries instead of popping over an unsaved pick. A failed write keeps the
//! page, shows the status and — for the account fields — a Retry row. BACK is held while an account
//! write is in flight, so the receipt always lands on a page that can publish it to its parent.
use super::*;
use crate::ui::decision_prompt::{DecisionPrompt, PromptStep};
use crate::ui::screen::{At, Dir, Focusable, GroupSpec, Placed, Step};
use super::super::family::ALERT_GROUP;
use super::super::registry::ALERT;

/// The Retry row's focus key: above any option position (the language list is a few hundred rows).
const RETRY_KEY: u32 = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
enum OptionId { Choice(Value), Retry }
#[derive(Clone, Debug)]
enum PickAction { Pick(Value), Retry }

struct PickerState {
    field: PickerKind, selected: u32, io: Io, quality: Quality, direct_play: DirectPlayMode,
    /// Position of the checked option (`u32::MAX` while none is).
    checked: u32, confirming: bool, affirmative: bool, alert_scroll: u32,
}
impl LogicalState for PickerState {
    fn write(&self, c: &mut Canon) {
        c.u8(self.field as u8).u32(self.selected).bool(self.io.busy).str(&self.io.status)
            .u8(self.quality as u8).u8(self.direct_play as u8).u32(self.checked)
            .bool(self.confirming).bool(self.affirmative).u32(self.alert_scroll);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("picker {:?} sel={} busy={} confirm={}", self.field, self.selected, self.io.busy, self.confirming));
    }
}

pub(crate) const SHAPE: &str = "PickerV1{field:u8,selection:u32,busy:bool,status:str,quality:u8,direct_play:u8,checked:u32,confirm:bool,affirm:bool,alert_scroll:u32}";

pub(crate) struct PickerPage {
    entry: EntryId,
    form: FormTable<OptionId, PickAction, SettingsPage>,
    state: PickerState,
    copy: String,
    txn: Txn,
    alert: DecisionPrompt,
}

/// The picker's rows as a [`Form`], from plain inputs: one `Choice` per option (checked when it is
/// `current`), then — only after a failed account write — the Retry button in its own section.
fn option_form(options: Vec<(String, Value)>, current: &Value, busy: bool, retry: bool) -> Form<OptionId, PickAction, SettingsPage> {
    let mut choices = FormSection::new("");
    for (i, (label, value)) in options.into_iter().enumerate() {
        let row = Row::new(&label).checked(value == *current).dim(busy);
        choices = choices.item_keyed(OptionId::Choice(value.clone()), RowKey(i as u32), RowKind::Choice, PickAction::Pick(value), row);
    }
    let retry = FormSection::new("").visible(retry).item_keyed(OptionId::Retry, RowKey(RETRY_KEY), RowKind::Button, PickAction::Retry,
        Row::new(crate::i18n::msg::settings_audio_retry()).detail(crate::i18n::msg::settings_audio_retry_detail()));
    Form::new().section(choices).section(retry)
}

impl PickerPage {
    pub(crate) fn new(entry: EntryId, field: PickerKind) -> Self {
        let mut s = Self { entry, form: FormTable::new(BAND),
            state: PickerState { field, selected: 0, io: Io { busy: false, status: String::new() },
                quality: crate::route::quality(), direct_play: crate::route::direct_play_mode(),
                checked: u32::MAX, confirming: false, affirmative: false, alert_scroll: 0 },
            copy: String::new(), txn: Txn::new(true),
            alert: DecisionPrompt::new(ALERT_GROUP, ALERT, ALERT + 1, crate::i18n::msg::settings_cancel_c(), crate::i18n::msg::settings_playback_enable_force_c()) };
        s.rebuild(true);
        s
    }
    fn current(&self) -> Value {
        resolve_value(self.state.field, self.state.quality, self.state.direct_play, self.txn.prefs())
    }
    /// Whether the STORED setting already is `value`. Only Size and Position can differ from
    /// [`Self::current`]: they publish the live value before the write lands, so after a failed
    /// write the checked row is live but not on disk, and OK on it must retry rather than pop.
    fn durable(&self, value: &Value) -> bool {
        match value {
            Value::SubtitleSize(v) => crate::plex::session::peek().subtitle_size() == *v,
            Value::SubtitlePosition(v) => crate::plex::session::peek().subtitle_position() == *v,
            _ => true,
        }
    }
    /// Position of the checked option right now. Size and Position publish their live value the
    /// moment a pick is made (before the write lands), so this can move without a receipt.
    fn checked_position(&self) -> u32 {
        let current = self.current();
        field_options(self.state.field, self.state.quality, self.state.direct_play, self.txn.prefs())
            .iter().position(|(_, v)| *v == current).map_or(u32::MAX, |i| i as u32)
    }
    fn view(&self) -> TableScreen<'_> {
        TableScreen::new(Header::new(RouteLayout::screen(), Some(self.state.field.kind().title()),
            self.state.field.title(), &self.copy), &self.form.table, GroupId(0), self.entry).keyed(&self.form)
    }
    fn focus(&self, fx: &mut Effects<'_, InnerHost>, group: GroupId) {
        fx.push(Fx::Deliver(MachineId::Instance(InstanceId(0)),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: if group == GroupId(0) {
                FocusTarget::Elem(FocusKey { entry: self.entry, elem: self.state.selected })
            } else { FocusTarget::ContainerGroup(group) } }))));
    }
    fn account(&self) -> bool { self.state.field.kind() == Kind::AudioSubtitles }
    fn start_initial_load(&mut self, fx: &mut Effects<'_, InnerHost>) -> bool {
        if self.account() && self.txn.request.is_none() && self.txn.snapshot.is_none()
            && self.txn.pending.is_none() && self.state.io.status.is_empty() {
            self.txn.load(&mut self.state.io, fx);
            true
        } else { false }
    }
    /// Rebuild the list. `seat_current` opens it on the checked option (construction, a snapshot
    /// landing); otherwise the cursor stays on its row by identity.
    fn rebuild(&mut self, seat_current: bool) {
        self.state.quality = crate::route::quality();
        self.state.direct_play = crate::route::direct_play_mode();
        self.copy = copy_text(Subject::Picker(self.state.field), &self.state.io.status, self.state.direct_play).into_owned();
        self.state.confirming = self.alert.is_open(); self.state.affirmative = self.alert.choice();
        self.state.alert_scroll = self.alert.scroll_target_bits();
        let current = self.current();
        let options = field_options(self.state.field, self.state.quality, self.state.direct_play, self.txn.prefs());
        self.state.checked = options.iter().position(|(_, v)| *v == current).map_or(u32::MAX, |i| i as u32);
        let keep = if seat_current { OptionId::Choice(current.clone()) } else {
            self.form.selected_id().cloned().unwrap_or_else(|| OptionId::Choice(current.clone())) };
        let show_retry = self.account() && !self.state.io.busy && !self.state.io.status.is_empty();
        self.form.table.compact = false; self.form.table.header_ink = theme::TEXT_READING; self.form.table.list_focused = true;
        self.form.set(option_form(options, &current, self.state.io.busy, show_retry), Some(&keep));
        self.state.selected = self.form.key_at(self.form.table.sel.max(0) as usize).map_or(0, |k| k.0);
    }
    fn pop(&self, fx: &mut Effects<'_, InnerHost>) {
        fx.push(Fx::Nav(crate::ui::machine::NavOp::Pop));
        fx.invalidate(crate::ui::present::Provenance::Input);
    }
    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        if self.state.io.busy { return; }
        match form_activate(&self.form, key, fx) {
            Some(PickAction::Pick(value)) => self.pick(value, fx),
            Some(PickAction::Retry) => {
                if let Some(update) = self.txn.retry.clone() { self.txn.start_account(&mut self.state.io, Some(update), fx); }
                else { self.txn.load(&mut self.state.io, fx); }
                self.rebuild(false);
                fx.invalidate(crate::ui::present::Provenance::Input);
            }
            None => {}
        }
    }
    fn pick(&mut self, value: Value, fx: &mut Effects<'_, InnerHost>) {
        // OK on the checked row is not a change. Compared against the CANONICAL current value, so a
        // stored deprecated code (`pb`) is not rewritten to `pt-BR` on the user's account by a pick
        // that changed nothing.
        if self.current() == value && self.durable(&value) { self.pop(fx); return; }
        if value == Value::DirectPlay(DirectPlayMode::Forced) && self.state.direct_play != DirectPlayMode::Forced {
            self.alert.open(crate::i18n::msg::settings_playback_force_question_c(), crate::i18n::msg::settings_playback_force_body());
            self.state.confirming = true; self.state.affirmative = false;
            self.state.alert_scroll = self.alert.scroll_target_bits(); self.focus(fx, ALERT_GROUP);
            fx.invalidate(crate::ui::present::Provenance::Input);
            return;
        }
        self.commit(value, fx);
        self.rebuild(false);
        fx.invalidate(crate::ui::present::Provenance::Input);
    }
    fn commit(&mut self, value: Value, fx: &mut Effects<'_, InnerHost>) {
        match value {
            Value::Quality(_) | Value::DirectPlay(_) | Value::NextEpisode(_) | Value::SkipInterval(_) | Value::SubtitleSize(_) | Value::SubtitlePosition(_) => self.txn.save_local(&mut self.state.io, value, fx),
            value => {
                let mut update = PreferenceUpdate::default();
                match (self.state.field, value) {
                    (PickerKind::AudioLanguage, Value::Language(v)) => {
                        update.audio_language = Some(v); update.auto_select_audio = Some(true);
                    }
                    (PickerKind::SubtitleLanguage, Value::Language(v)) => update.subtitle_language = Some(v),
                    (_, Value::Mode(v)) => {
                        update.subtitle_mode = Some(v);
                        if v > 0 { update.auto_select_audio = Some(true); }
                    },
                    (_, Value::Forced(v)) => update.subtitle_forced = Some(v),
                    _ => return,
                }
                self.txn.start_account(&mut self.state.io, Some(update), fx);
            }
        }
    }
    /// An account write is in flight: leaving now would orphan its receipt.
    fn writing_account(&self) -> bool {
        self.txn.saving && matches!(self.txn.pending, Some(Pending::Account(_)))
    }
}

impl Machine<InnerHost> for PickerPage {
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
                self.state.confirming = false;
                if yes {
                    self.txn.save_local(&mut self.state.io, Value::DirectPlay(DirectPlayMode::Forced), fx);
                    self.rebuild(false);
                }
                self.focus(fx, GroupId(0));
                fx.invalidate(crate::ui::present::Provenance::Input); return Handled::Yes;
            }
        }
        match ev {
            ScreenEvent::Enter(_) => {
                if self.start_initial_load(fx) { self.rebuild(true); fx.invalidate(crate::ui::present::Provenance::Input); }
                Handled::No
            }
            ScreenEvent::Tick(t) => {
                self.alert.update(t.dt());
                let started = self.start_initial_load(fx);
                if self.txn.stale() {
                    // The profile changed under the list: nothing here can be saved any more. Leave;
                    // the field list beneath sees the same staleness and reloads.
                    self.txn.snapshot = None; self.txn.pending = None; self.state.io.busy = false;
                    self.pop(fx);
                    return Handled::Yes;
                }
                let had_rows = self.form.table.n_rows() > 0;
                let landed = self.txn.poll(&mut self.state.io, fx);
                match landed {
                    Landed::Saved => { self.pop(fx); }
                    Landed::Changed => {
                        let failed = !self.state.io.status.is_empty();
                        self.rebuild(self.txn.snapshot.is_some() && !failed);
                        // A failed account write offers Retry: put the cursor on it.
                        if failed && self.form.index_of_key(RowKey(RETRY_KEY)).is_some() {
                            self.form.table.sel = self.form.index_of_key(RowKey(RETRY_KEY)).unwrap_or(0) as i32;
                            self.state.selected = RETRY_KEY;
                            self.focus(fx, GroupId(0));
                        } else if !had_rows && cx.focus.current.is_none() { self.focus(fx, GroupId(0)); }
                        fx.invalidate(crate::ui::present::Provenance::Landing(MachineId::Session));
                    }
                    // an optimistic Size/Position pick published its value: move the checkmark now
                    Landed::Nothing if !started && self.checked_position() != self.state.checked => {
                        self.rebuild(false);
                        fx.invalidate(crate::ui::present::Provenance::Landing(MachineId::Session));
                    }
                    Landed::Nothing if started => { self.rebuild(true); fx.invalidate(crate::ui::present::Provenance::Landing(MachineId::Session)); }
                    Landed::Nothing => {}
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
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. }, .. }) if self.writing_account() => Handled::Yes,
            _ => Handled::No,
        }
    }
}

impl Focusable<InnerHost> for PickerPage {
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
            return FocusKey { entry: self.entry, elem: self.state.selected };
        }
        Focusable::<InnerHost>::reconcile(&self.view(), want, cx)
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        match self.alert.seat(g, self.entry) {
            Some(key) => key,
            // A list is entered on its CHECKED option (the cursor the page opened on), wherever the
            // entry comes from — the push's `FirstInGroup` included, which a page-emitted Enter
            // cannot outrun (the surface queues its own after the page's mount effects).
            None if g == GroupId(0) && self.form.table.n_rows() > 0 =>
                FocusKey { entry: self.entry, elem: self.state.selected },
            None => Focusable::<InnerHost>::seat(&self.view(), g, from, cx),
        }
    }
}

impl Screen<InnerHost> for PickerPage {
    fn name(&self) -> &'static str { word::PICKER }
    fn state(&self) -> &dyn LogicalState { &self.state }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(crate::i18n::msg::settings_title()))
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        crate::ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL);
        self.alert.draw(f, self.entry);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
}

#[cfg(test)]
#[path = "picker_tests.rs"]
mod tests;
