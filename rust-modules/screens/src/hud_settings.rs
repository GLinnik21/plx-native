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
//! **Install from address.** One more row, *Install from address*: OK raises the television keyboard (the Search page's
//! mechanism: a `Delivery::Keyboard`, then the OS's `InputKind::Text` edits into a [`TextBuffer`]) and the row's detail
//! line shows what was typed. OK on the keyboard parses it (`hudpacks::address::parse`); a refusal is the line under the
//! title, a good address leaves as `HudCmd::Fetch`. The store fetches, validates and *offers* the package: the next
//! listing carries it as `HudList::offer` and the page asks the same two-answer prompt a drop file gets, with the
//! package's name and version, where it came from and *Unreviewed*, Cancel focused. Confirming sends `InstallOffer`,
//! anything else `DeclineOffer`, which leaves nothing on disk. While the fetch runs the row is dimmed and OK on it does
//! nothing (the Preferences pages' busy idiom).
//!
//! **What a row says.** The package's `name` (the author's text: a server-style label, never measured by
//! the fit report), then its trust tier (*Built-in*, *Unreviewed*) and one more fact: why it cannot run (a
//! format the app does not read, an app range it is outside of, or "Doesn't fit in {language}": a compile the
//! television refused in the current language, where classic draws and the selection is kept), else "Installed
//! until restart" when only the volatile directory holds it. One fact is what the detail line fits in every
//! language.
use std::borrow::Cow;
use std::sync::mpsc::{self, Receiver};

use plx_data::hudpacks::address::{self, AddressError};
use plx_data::hudpacks::{DropFile, Entry, FetchError, Notice, OfferView, Refusal, Sha, Trust, Unloadable, BUILTIN_CLASSIC};
use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, InputOwner, InstanceId, Key, LogicalState,
    Machine, MachineId, TextEdit,
};
use plx_ui::decision_prompt::{DecisionPrompt, PromptStep};
use plx_ui::text_buffer::TextBuffer;
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
    /// A package fetched from an address, validated and waiting for the viewer's answer: the prompt shows it.
    pub offer: Option<OfferView>,
    /// A fetch from an address is in flight (the row is busy and takes no second request).
    pub fetching: bool,
}

/// A row's id: what it IS, so the cursor keeps its place across a re-listing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HudId {
    Pack(String),
    Remove(String),
    Drop(Sha),
    /// The *Install from address* row.
    Address,
    /// A file the store could not hash (over the size bound): its position in the folder listing.
    DropFile(usize),
}

#[derive(Clone, Debug)]
enum HudAction {
    Select(String),
    Remove(String),
    Install(Sha),
    /// Raise the keyboard for an address.
    Address,
    /// A row with nothing to do: a file that cannot be installed and has no hash to name.
    Inert,
}

/// Row keys: a package's position, then the removals and the drop files in their own ranges.
const REMOVE_KEY: u32 = 0x1000;
const DROP_KEY: u32 = 0x2000;
const ADDRESS_KEY: u32 = 0x3000;

/// What the open prompt asks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asking {
    /// A file in the drop folder.
    Drop(Sha),
    /// A package fetched from an address and held by the store.
    Offer(Sha),
}

struct State {
    selected: u32,
    rows: u32,
    notice: u8,
    confirming: bool,
    affirmative: bool,
    alert_scroll: u32,
    /// The address field has the keyboard.
    editing: bool,
    /// A fetch is in flight (the list says so).
    fetching: bool,
}
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.u32(self.selected).u32(self.rows).u8(self.notice).bool(self.confirming).bool(self.affirmative).u32(self.alert_scroll)
            .bool(self.editing).bool(self.fetching);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "player-hud sel={} rows={} confirm={} editing={} fetching={}",
            self.selected, self.rows, self.confirming, self.editing, self.fetching
        ));
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
    /// What the open prompt asks about.
    asking: Option<Asking>,
    /// The offer the prompt was last opened for, so a listing that still carries it after the answer does not reopen it.
    offered: Option<Sha>,
    /// What the viewer has typed into the address row; kept across a refusal so it can be corrected.
    draft: TextBuffer,
    /// Why the last submitted address was refused, until the next edit.
    address_error: Option<AddressError>,
    prompt: DecisionPrompt,
}

/// The crumb over the title: the page this one is pushed from, Video & playback (the Playback page's Player HUD row
/// opens it), as `PickerKind::parent_title` names it for a picker on that page.
fn parent_title() -> &'static str {
    plx_platform::i18n::msg::settings_playback_title()
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
        Refusal::Invalid => plx_platform::i18n::msg::settings_hud_not_a_hud(),
        Refusal::TooLarge => plx_platform::i18n::msg::settings_hud_too_large(),
        Refusal::BuiltinId | Refusal::Changed => plx_platform::i18n::msg::settings_hud_cant_install(),
    }
}

/// Why a fetch from an address brought nothing, in the viewer's words.
fn fetch_text(why: FetchError) -> &'static str {
    match why {
        FetchError::NotFound => plx_platform::i18n::msg::settings_hud_fetch_not_found(),
        FetchError::TooLarge => plx_platform::i18n::msg::settings_hud_too_large(),
        FetchError::Unreachable => plx_platform::i18n::msg::settings_hud_fetch_unreachable(),
        FetchError::Certificate => plx_platform::i18n::msg::settings_hud_fetch_certificate(),
    }
}

/// Why the typed address was refused: the accepted forms, or the one rule a plain `http://` address broke.
fn address_text(why: AddressError) -> &'static str {
    match why {
        AddressError::HttpNotPrivate => plx_platform::i18n::msg::settings_hud_address_lan(),
        _ => plx_platform::i18n::msg::settings_hud_address_form(),
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
fn hud_form(list: &HudList, draft: Option<&str>) -> Form<HudId, HudAction, SettingsPage> {
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
    // The address row: the keyboard's text while it is up, "Downloading…" while a fetch runs, else what to type.
    let detail = match draft {
        Some(text) => format!("{text}|"),
        None if list.fetching => plx_platform::i18n::msg::settings_hud_fetching().to_owned(),
        None => plx_platform::i18n::msg::settings_hud_address_hint().to_owned(),
    };
    let row = Row::new(plx_platform::i18n::msg::settings_hud_address()).detail(detail).dim(list.fetching);
    let address = FormSection::new("").item_keyed(HudId::Address, RowKey(ADDRESS_KEY), RowKind::Button, HudAction::Address, row);
    let mut folder = FormSection::new(plx_platform::i18n::msg::settings_hud_folder()).visible(!list.drops.is_empty());
    for (i, file) in list.drops.iter().enumerate() {
        let name = file.meta.as_ref().map_or_else(|| file.name.clone(), |m| m.name.clone());
        let detail = match file.status {
            Ok(()) => join(&[plx_platform::i18n::msg::settings_hud_install().to_owned(), plx_platform::i18n::msg::settings_hud_unreviewed().to_owned()]),
            Err(why) => refusal_text(why).to_owned(),
        };
        let row = Row::new(&name).server_label().detail(detail);
        let (id, action) = match file.sha {
            Some(sha) => (HudId::Drop(sha), HudAction::Install(sha)),
            None => (HudId::DropFile(i), HudAction::Inert),
        };
        folder = folder.item_keyed(id, RowKey(DROP_KEY + i as u32), RowKind::Button, action, row).disabled(file.status.is_err());
    }
    Form::new().section(packs).section(removals).section(address).section(folder)
}

/// What the line under the title says: the last notice, else what the page is.
fn copy_of(list: &HudList, refused: Option<AddressError>) -> String {
    if let Some(why) = refused {
        return address_text(why).to_owned();
    }
    match &list.notice {
        Some(Notice::Refused { reason, .. }) => refusal_text(*reason).to_owned(),
        Some(Notice::FetchFailed { reason }) => fetch_text(*reason).to_owned(),
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
            state: State { selected: 0, rows: 0, notice: 0, confirming: false, affirmative: false, alert_scroll: 0, editing: false, fetching: false },
            list: HudList::default(),
            copy: String::new(),
            rx,
            tx: Some(tx),
            asking: None,
            offered: None,
            draft: TextBuffer::new(String::new(), 0),
            address_error: None,
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
            Header::new(RouteLayout::screen(), Some(parent_title()), plx_platform::i18n::msg::settings_hud_title(), &self.copy),
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
        self.copy = copy_of(&self.list, self.address_error);
        self.form.table.compact = false;
        self.form.table.header_ink = theme::TEXT_READING;
        self.form.table.list_focused = true;
        self.form.set(hud_form(&self.list, self.state.editing.then(|| self.draft.text())), keep.as_ref());
        self.state.fetching = self.list.fetching;
        self.state.rows = self.form.table.n_rows() as u32;
        self.state.notice = match &self.list.notice {
            None => 0,
            Some(Notice::Installed { .. }) => 1,
            Some(Notice::Refused { .. }) => 2,
            Some(Notice::NotWritten { .. }) => 3,
            Some(Notice::Removed { .. }) => 4,
            Some(Notice::RemoveIncomplete { .. }) => 5,
            Some(Notice::ReplacedUnreadable { .. }) => 6,
            Some(Notice::FetchFailed { .. }) => 7,
        };
        self.state.selected = self.form.key_at(self.form.table.sel.max(0) as usize).map_or(0, |k| k.0);
    }

    /// Open the page's channel on the first event, so a page that never gets one asks for nothing.
    fn open(&mut self, fx: &mut Effects<'_, InnerHost>) {
        if let Some(tx) = self.tx.take() {
            fx.push(Fx::App(AppFx::Huds(HudCmd::Open(tx))));
        }
    }

    /// Open the two-answer prompt about `asking`: `paragraphs` under `question`, Cancel focused.
    fn ask(&mut self, asking: Asking, question: &'static std::ffi::CStr, paragraphs: Vec<Cow<'static, str>>, fx: &mut Effects<'_, InnerHost>) {
        self.asking = Some(asking);
        self.prompt.open_paragraphs(question, paragraphs);
        self.state.confirming = true;
        self.state.affirmative = false;
        self.state.alert_scroll = self.prompt.scroll_target_bits();
        self.focus(fx, ALERT_GROUP);
    }

    /// Ask about the package the store holds from an address when the listing brings a new one: its name and version,
    /// where it came from (and that a plain `http://` source is a development one), *Unreviewed*, and what installing does.
    fn sync_offer(&mut self, fx: &mut Effects<'_, InnerHost>) {
        let Some(offer) = self.list.offer.clone() else {
            self.offered = None;
            return;
        };
        if self.offered == Some(offer.sha) {
            return;
        }
        self.offered = Some(offer.sha);
        let mut facts = Vec::new();
        if let Some(from) = &offer.from {
            facts.push(plx_platform::i18n::msg::settings_hud_offer_from(from.host()));
            if from.dev_source() {
                facts.push(plx_platform::i18n::msg::settings_hud_offer_dev().to_owned());
            }
        }
        facts.push(plx_platform::i18n::msg::settings_hud_unreviewed().to_owned());
        let paragraphs = vec![
            Cow::Owned(format!("{} {}", offer.meta.name, offer.meta.version)),
            Cow::Owned(join(&facts)),
            Cow::Borrowed(plx_platform::i18n::msg::settings_hud_offer_body()),
        ];
        self.ask(Asking::Offer(offer.sha), plx_platform::i18n::msg::settings_hud_install_question_c(), paragraphs, fx);
    }

    /// Put the keyboard down (its text stays in the draft) and show the row as it is without it.
    fn leave_field(&mut self, fx: &mut Effects<'_, InnerHost>) {
        if !self.state.editing {
            return;
        }
        self.state.editing = false;
        fx.push(Fx::Deliver(MachineId::Instance(InstanceId(0)), Delivery::Keyboard { up: false }));
        self.rebuild(Some(HudId::Address));
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    /// Edit the draft; any edit clears the refusal of the address before it.
    fn edit(&mut self, edit: &TextEdit, fx: &mut Effects<'_, InnerHost>) {
        self.draft.edit(edit);
        self.address_error = None;
        self.rebuild(Some(HudId::Address));
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    /// OK on the keyboard: a good address leaves as a fetch, a refused one is said under the title.
    fn submit(&mut self, fx: &mut Effects<'_, InnerHost>) {
        match address::parse(self.draft.text()) {
            Ok(address) => {
                self.address_error = None;
                self.leave_field(fx);
                fx.push(Fx::App(AppFx::Huds(HudCmd::Fetch(address))));
            }
            Err(why) => {
                self.address_error = Some(why);
                self.leave_field(fx);
            }
        }
    }

    /// Input while the address field has the keyboard (or the OS reports it): the OS's text edits, the remote's
    /// Left/Right/Backspace, OK to submit and Back to put the keyboard down. Every other key is the field's, not focus travel.
    fn typing(&mut self, kind: &InputKind<u32>, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match kind {
            // The television took its keyboard down by itself: the field closes with what was typed kept.
            InputKind::SystemKeyboard(up) => {
                if !*up && self.state.editing {
                    self.state.editing = false;
                    self.rebuild(Some(HudId::Address));
                    fx.invalidate(plx_machine::present::Provenance::Input);
                }
                Handled::Yes
            }
            InputKind::Text(edit) if self.state.editing || matches!(cx.owner, InputOwner::System(_)) => {
                self.edit(edit, fx);
                Handled::Yes
            }
            InputKind::Key { key, sym, edge, .. } if self.state.editing => {
                if *edge == Edge::Up {
                    return Handled::Yes;
                }
                let edit = match (*key, *sym) {
                    (Key::Left, _) => Some(TextEdit::Left),
                    (Key::Right, _) => Some(TextEdit::Right),
                    (_, plx_ui::consts::SDLK_BACKSPACE) => Some(TextEdit::Backspace),
                    (_, plx_ui::consts::SDLK_CLEAR) => Some(TextEdit::Clear),
                    _ => None,
                };
                match (edit, *key) {
                    (Some(edit), _) => self.edit(&edit, fx),
                    (None, Key::Back) => self.leave_field(fx),
                    (None, Key::Ok) => self.submit(fx),
                    _ => {}
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        match form_activate(&self.form, key, fx) {
            Some(HudAction::Select(id)) => fx.push(Fx::App(AppFx::Huds(HudCmd::Select(id)))),
            Some(HudAction::Remove(id)) => fx.push(Fx::App(AppFx::Huds(HudCmd::Remove(id)))),
            Some(HudAction::Install(sha)) => {
                self.ask(
                    Asking::Drop(sha),
                    plx_platform::i18n::msg::settings_hud_install_question_c(),
                    vec![Cow::Borrowed(plx_platform::i18n::msg::settings_hud_install_body())],
                    fx,
                );
            }
            // A fetch in flight takes no second request: the row is dimmed and OK on it does nothing.
            Some(HudAction::Address) if !self.list.fetching && !self.state.editing => {
                self.state.editing = true;
                self.address_error = None;
                let text = self.draft.text().to_owned();
                let end = text.len();
                self.draft = TextBuffer::new(text, end);
                fx.push(Fx::Deliver(MachineId::Instance(InstanceId(0)), Delivery::Keyboard { up: true }));
                self.rebuild(Some(HudId::Address));
            }
            Some(HudAction::Address) | Some(HudAction::Inert) | None => {}
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
                match self.asking.take() {
                    Some(Asking::Drop(sha)) if yes => fx.push(Fx::App(AppFx::Huds(HudCmd::InstallDrop(sha)))),
                    Some(Asking::Offer(sha)) => {
                        fx.push(Fx::App(AppFx::Huds(if yes { HudCmd::InstallOffer(sha) } else { HudCmd::DeclineOffer(sha) })));
                    }
                    _ => {}
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
                    self.sync_offer(fx);
                    if !had_rows && self.form.table.n_rows() > 0 && cx.focus.current.is_none() {
                        self.focus(fx, GroupId(0));
                    }
                    fx.invalidate(plx_machine::present::Provenance::Landing(MachineId::Session));
                }
                self.form.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind, .. }) if self.state.editing || matches!(kind, InputKind::SystemKeyboard(_)) => self.typing(kind, cx, fx),
            ScreenEvent::WillLeave(_) | ScreenEvent::Unmount | ScreenEvent::Cover | ScreenEvent::Suspend => {
                self.leave_field(fx);
                Handled::No
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
        Some(Cow::Borrowed(parent_title()))
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
    form.set(hud_form(list, None), None);
    (0..form.table.n_rows() as usize).filter_map(|i| Some((form.key_at(i)?.0, form.binding_at(i)?.id.clone()))).collect()
}

#[cfg(test)]
#[path = "hud_settings_tests.rs"]
mod tests;
