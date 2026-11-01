//! **Settings > Playback > Player HUD** (`docs/hud-plugins.md`, "Storage per install mode", "Sideload"):
//! the built-in HUD, the installed packages and the files in the drop folder, on the shared Settings
//! table. A FormTable keyed by package id, not a `PickerKind` (the vocabulary is whatever is installed).
//!
//! **The page owns no store and no IO.** Everything it shows is a [`HudList`] the Bridge sends down the
//! channel its `HudCmd::Open` carried (the page's own data, like the receipts the Playback pickers
//! poll), and everything it asks leaves as one `AppFx::Huds`. OK on a package picks it
//! (`HudCmd::Select`), OK on a drop file asks, through the shared two-answer prompt
//! ([`DecisionPrompt`]), whether to install an unreviewed file, and the *Remove* row of an installed package
//! deletes it. The list is read again each time the page opens.
//!
//! **What a row says.** The package's `name` (the author's text: a server-style label, never measured by
//! the fit report), then the trust tier (*Built-in*, *Unreviewed*), "Installed until restart" when only
//! the volatile directory holds it, and the reason it cannot run: a format the app does not read, an
//! app range it is outside of, or ("Doesn't fit in {language}") a compile the television refused in the
//! current language, where classic draws and the selection is kept.
use std::borrow::Cow;
use std::sync::mpsc::{self, Receiver};

use plx_data::hudpacks::{DropFile, Entry, Notice, Refusal, Sha, Trust, Unloadable, BUILTIN_CLASSIC};
use plx_machine::machine::{Canon, Cx, Delivery, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InstanceId, LogicalState, Machine, MachineId};
use plx_ui::decision_prompt::{DecisionPrompt, PromptStep};
use plx_ui::form::{Form, FormSection, FormTable, RowKey, RowKind};
use plx_ui::frame::Budget;
use plx_ui::route_screen::RouteLayout;
use plx_ui::screen::{At, Dir, DrawFrame, Enter, Focusable, FocusSource, FocusTarget, GroupSpec, HitSource, Placed, RenderStrategy, Screen, ScreenEvent, Step};
use plx_ui::table::Row;
use plx_ui::table_screen::{Header, TableScreen};
use plx_ui::{theme, Rect};

use super::family::{form_activate, form_focus, form_right_target, InnerHost, SettingsPage, ALERT_GROUP};
use super::registry::{word, AppFx, HudCmd, ALERT, BAND};

/// What the Bridge publishes to the page: the store's listing at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HudList {
    /// The built-in first, then one row per installed id.
    pub entries: Vec<Entry>,
    /// The files in the drop folder.
    pub drops: Vec<DropFile>,
    /// The packages whose compile the television refused in the current language.
    pub unfit: Vec<Sha>,
    /// The current language's own name, for "Doesn't fit in {language}".
    pub language: String,
    /// What the last install or removal said, until the page is opened again.
    pub notice: Option<Notice>,
}

/// A row's id: what it IS, so the cursor keeps its place across a re-listing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HudId {
    Pack(String),
    Remove(String),
    Drop(Sha),
}

#[derive(Clone, Debug)]
enum HudAction {
    Select(String),
    Remove(String),
    Install(Sha),
}

/// Row keys: a package's position, then the removals and the drop files in their own ranges.
const REMOVE_KEY: u32 = 0x1000;
const DROP_KEY: u32 = 0x2000;

struct State {
    selected: u32,
    rows: u32,
    notice: u8,
    confirming: bool,
    affirmative: bool,
    alert_scroll: u32,
}
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.u32(self.selected).u32(self.rows).u8(self.notice).bool(self.confirming).bool(self.affirmative).u32(self.alert_scroll);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("player-hud sel={} rows={} confirm={}", self.selected, self.rows, self.confirming));
    }
}

pub struct HudPage {
    entry: EntryId,
    form: FormTable<HudId, HudAction, SettingsPage>,
    state: State,
    list: HudList,
    copy: String,
    /// The list's channel: the sender went to the Bridge with `HudCmd::Open`.
    rx: Receiver<HudList>,
    tx: Option<mpsc::Sender<HudList>>,
    /// The drop file the open prompt asks about.
    asking: Option<Sha>,
    prompt: DecisionPrompt,
}

/// The package row's name: the built-in is the app's word, any other the author's text.
fn name_of(entry: &Entry) -> String {
    match &entry.meta {
        Some(meta) => meta.name.clone(),
        None => plx_platform::i18n::msg::settings_hud_classic().to_owned(),
    }
}

/// Why `unloadable` says this package cannot run, in the viewer's words.
fn unloadable_text(why: Unloadable) -> &'static str {
    match why {
        Unloadable::NewerFormat => plx_platform::i18n::msg::settings_hud_newer_format(),
        Unloadable::OlderFormat => plx_platform::i18n::msg::settings_hud_older_format(),
        Unloadable::AppRange => plx_platform::i18n::msg::settings_hud_app_range(),
    }
}

/// Why a file in the drop folder cannot be installed now.
fn refusal_text(why: Refusal) -> &'static str {
    match why {
        Refusal::Unloadable(why) => unloadable_text(why),
        Refusal::AlreadyInstalled | Refusal::NotNewer => plx_platform::i18n::msg::settings_hud_already(),
        Refusal::BuiltinId | Refusal::Invalid | Refusal::TooLarge | Refusal::Changed => plx_platform::i18n::msg::settings_hud_cant_install(),
    }
}

fn join(parts: &[String]) -> String {
    parts.join(" \u{b7} ")
}

/// What a package row says under its name: its trust tier and ONE more fact: why it cannot run (a format or
/// app range the app does not accept, or a compile the television refused in the current language), else
/// "Installed until restart" when only the volatile directory holds it.
fn facts_of(entry: &Entry, list: &HudList) -> Vec<String> {
    let mut facts = Vec::new();
    match entry.trust {
        Trust::BuiltIn => facts.push(plx_platform::i18n::msg::settings_hud_builtin().to_owned()),
        Trust::Unreviewed => facts.push(plx_platform::i18n::msg::settings_hud_unreviewed().to_owned()),
        Trust::Registry => {}
    }
    // Two facts fit a row's detail line in every language; three do not (French and Belarusian with the
    // longest language name), so the reason a package cannot run outranks "installed until restart".
    match entry.loadable {
        Err(why) => facts.push(unloadable_text(why).to_owned()),
        Ok(()) if entry.sha.is_some_and(|sha| list.unfit.contains(&sha)) => {
            facts.push(plx_platform::i18n::msg::settings_hud_unfit_language(&list.language));
        }
        Ok(()) if entry.until_restart => facts.push(plx_platform::i18n::msg::settings_hud_until_restart().to_owned()),
        Ok(()) => {}
    }
    facts
}

/// The page's rows from the list alone: the packages (the built-in first), a removal row for each installed
/// one, and the drop files under their own header when there are any.
fn hud_form(list: &HudList) -> Form<HudId, HudAction, SettingsPage> {
    let mut packs = FormSection::new("");
    let mut removals = FormSection::new("");
    for (i, entry) in list.entries.iter().enumerate() {
        let name = name_of(entry);
        let mut row = Row::new(&name).checked(entry.selected).detail(join(&facts_of(entry, list))).dim(entry.loadable.is_err());
        if entry.meta.is_some() {
            row = row.server_label();
        }
        packs = packs.item_keyed(HudId::Pack(entry.id.clone()), RowKey(i as u32), RowKind::Choice, HudAction::Select(entry.id.clone()), row)
            .disabled(entry.loadable.is_err());
        if entry.id != BUILTIN_CLASSIC {
            let row = Row::new(&name).server_label().detail(plx_platform::i18n::msg::settings_hud_remove());
            removals = removals.item_keyed(HudId::Remove(entry.id.clone()), RowKey(REMOVE_KEY + i as u32), RowKind::Button, HudAction::Remove(entry.id.clone()), row);
        }
    }
    let mut folder = FormSection::new(plx_platform::i18n::msg::settings_hud_folder()).visible(!list.drops.is_empty());
    for (i, file) in list.drops.iter().enumerate() {
        let Some(sha) = file.sha else { continue };
        let name = file.meta.as_ref().map_or_else(|| file.name.clone(), |m| m.name.clone());
        let detail = match file.status {
            Ok(()) => join(&[plx_platform::i18n::msg::settings_hud_install().to_owned(), plx_platform::i18n::msg::settings_hud_unreviewed().to_owned()]),
            Err(why) => refusal_text(why).to_owned(),
        };
        let row = Row::new(&name).server_label().detail(detail);
        folder = folder.item_keyed(HudId::Drop(sha), RowKey(DROP_KEY + i as u32), RowKind::Button, HudAction::Install(sha), row)
            .disabled(file.status.is_err());
    }
    Form::new().section(packs).section(removals).section(folder)
}

/// What the line under the title says: the last notice, else what the page is.
fn copy_of(list: &HudList) -> String {
    match &list.notice {
        Some(Notice::Refused { reason, .. }) => refusal_text(*reason).to_owned(),
        Some(Notice::NotWritten { .. }) => plx_platform::i18n::msg::settings_hud_not_saved().to_owned(),
        _ => plx_platform::i18n::msg::settings_hud_copy().to_owned(),
    }
}

impl HudPage {
    pub fn new(entry: EntryId) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut page = Self {
            entry,
            form: FormTable::new(BAND),
            state: State { selected: 0, rows: 0, notice: 0, confirming: false, affirmative: false, alert_scroll: 0 },
            list: HudList::default(),
            copy: String::new(),
            rx,
            tx: Some(tx),
            asking: None,
            prompt: DecisionPrompt::new(
                ALERT_GROUP,
                ALERT,
                ALERT + 1,
                plx_platform::i18n::msg::settings_cancel_c(),
                plx_platform::i18n::msg::settings_hud_install_c(),
            ),
        };
        page.rebuild(None);
        page
    }

    fn view(&self) -> TableScreen<'_> {
        TableScreen::new(
            Header::new(RouteLayout::screen(), Some(plx_platform::i18n::msg::settings_title()), plx_platform::i18n::msg::settings_hud_title(), &self.copy),
            &self.form.table,
            GroupId(0),
            self.entry,
        )
        .keyed(&self.form)
    }

    fn focus(&self, fx: &mut Effects<'_, InnerHost>, group: GroupId) {
        fx.push(Fx::Deliver(
            MachineId::Instance(InstanceId(0)),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                focus: if group == GroupId(0) {
                    FocusTarget::Elem(FocusKey { entry: self.entry, elem: self.state.selected })
                } else {
                    FocusTarget::ContainerGroup(group)
                },
            })),
        ));
    }

    /// Build the rows from `self.list`, keeping the cursor on its row by identity.
    fn rebuild(&mut self, keep: Option<HudId>) {
        self.copy = copy_of(&self.list);
        self.form.table.compact = false;
        self.form.table.header_ink = theme::TEXT_READING;
        self.form.table.list_focused = true;
        self.form.set(hud_form(&self.list), keep.as_ref());
        self.state.rows = self.form.table.n_rows() as u32;
        self.state.notice = match &self.list.notice {
            None => 0,
            Some(Notice::Installed { .. }) => 1,
            Some(Notice::Refused { .. }) => 2,
            Some(Notice::NotWritten { .. }) => 3,
            Some(Notice::Removed { .. }) => 4,
            Some(Notice::RemoveIncomplete { .. }) => 5,
            Some(Notice::ReplacedUnreadable { .. }) => 6,
        };
        self.state.selected = self.form.key_at(self.form.table.sel.max(0) as usize).map_or(0, |k| k.0);
    }

    /// Open the page's channel on the first event, so a page that never gets one asks for nothing.
    fn open(&mut self, fx: &mut Effects<'_, InnerHost>) {
        if let Some(tx) = self.tx.take() {
            fx.push(Fx::App(AppFx::Huds(HudCmd::Open(tx))));
        }
    }

    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        match form_activate(&self.form, key, fx) {
            Some(HudAction::Select(id)) => fx.push(Fx::App(AppFx::Huds(HudCmd::Select(id)))),
            Some(HudAction::Remove(id)) => fx.push(Fx::App(AppFx::Huds(HudCmd::Remove(id)))),
            Some(HudAction::Install(sha)) => {
                self.asking = Some(sha);
                self.prompt.open(plx_platform::i18n::msg::settings_hud_install_question_c(), plx_platform::i18n::msg::settings_hud_install_body());
                self.state.confirming = true;
                self.state.affirmative = false;
                self.state.alert_scroll = self.prompt.scroll_target_bits();
                self.focus(fx, ALERT_GROUP);
            }
            None => {}
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }
}

impl Machine<InnerHost> for HudPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match self.prompt.step(ev, cx) {
            PromptStep::Pass => (),
            PromptStep::Done(handled) => {
                self.state.affirmative = self.prompt.choice();
                self.state.alert_scroll = self.prompt.scroll_target_bits();
                return handled;
            }
            PromptStep::Answer(yes) => {
                self.state.confirming = false;
                if let Some(sha) = self.asking.take().filter(|_| yes) {
                    fx.push(Fx::App(AppFx::Huds(HudCmd::InstallDrop(sha))));
                }
                self.focus(fx, GroupId(0));
                fx.invalidate(plx_machine::present::Provenance::Input);
                return Handled::Yes;
            }
        }
        match ev {
            ScreenEvent::Enter(_) => {
                self.open(fx);
                Handled::No
            }
            ScreenEvent::Tick(t) => {
                self.open(fx);
                self.prompt.update(t.dt());
                let had_rows = self.form.table.n_rows() > 0;
                let mut latest = None;
                while let Ok(list) = self.rx.try_recv() {
                    latest = Some(list);
                }
                if let Some(list) = latest.filter(|l| *l != self.list) {
                    self.list = list;
                    let keep = self.form.selected_id().cloned();
                    self.rebuild(keep);
                    if !had_rows && self.form.table.n_rows() > 0 && cx.focus.current.is_none() {
                        self.focus(fx, GroupId(0));
                    }
                    fx.invalidate(plx_machine::present::Provenance::Landing(MachineId::Session));
                }
                self.form.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                self.state.selected = self.form.key_at(self.form.table.sel.max(0) as usize).map_or(0, |k| k.0);
                Handled::Yes
            }
            ScreenEvent::Activate(key) => {
                self.activate(*key, fx);
                Handled::Yes
            }
            ScreenEvent::Input(plx_machine::machine::InputEvent {
                kind: plx_machine::machine::InputKind::Key { key: plx_machine::machine::Key::Right, at_edge: true, .. },
                ..
            }) => {
                if let Some(key) = cx.focus.current.and_then(|k| form_right_target(&self.form, k.elem)) {
                    self.activate(key, fx);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl Focusable<InnerHost> for HudPage {
    fn groups(&self, cx: &Cx<'_, InnerHost>, out: &mut Vec<GroupSpec>) {
        if !self.prompt.groups(out, cx.measure) {
            Focusable::<InnerHost>::groups(&self.view(), cx, out)
        }
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, InnerHost>) -> Option<GroupId> {
        match self.prompt.group_of(*key) {
            Some(answer) => answer,
            None => Focusable::<InnerHost>::group_of(&self.view(), key, cx),
        }
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, InnerHost>) -> Step<u32> {
        match self.prompt.neighbour(key, dir) {
            Some(step) => step,
            None => Focusable::<InnerHost>::neighbour(&self.view(), key, dir, cx),
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, InnerHost>, at: At) -> Option<Placed> {
        match self.prompt.place(*key, cx.measure) {
            Some(placed) => placed,
            None => Focusable::<InnerHost>::place(&self.view(), key, cx, at),
        }
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        if let Some(key) = self.prompt.reconcile(want) {
            return key;
        }
        if self.prompt.owns(want.elem) {
            return FocusKey { entry: self.entry, elem: self.state.selected };
        }
        Focusable::<InnerHost>::reconcile(&self.view(), want, cx)
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        match self.prompt.seat(g, self.entry) {
            Some(key) => key,
            None => Focusable::<InnerHost>::seat(&self.view(), g, from, cx),
        }
    }
}

impl Screen<InnerHost> for HudPage {
    fn name(&self) -> &'static str {
        word::PLAYER_HUD
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(plx_platform::i18n::msg::settings_title()))
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        plx_ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL);
        self.prompt.draw(f, self.entry);
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

/// The `(focus key, id)` of every row `list` shows, for a test: the page's own builder.
#[cfg(test)]
fn rows_for_test(list: &HudList) -> Vec<(u32, HudId)> {
    let mut form: FormTable<HudId, HudAction, SettingsPage> = FormTable::new(BAND);
    form.set(hud_form(list), None);
    (0..form.table.n_rows() as usize).filter_map(|i| Some((form.key_at(i)?.0, form.binding_at(i)?.id.clone()))).collect()
}

#[cfg(test)]
#[path = "hud_settings_tests.rs"]
mod tests;
