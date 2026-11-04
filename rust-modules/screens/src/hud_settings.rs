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
    Machine, MachineId, TextEdit, NavOp,
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
    pub catalog: plx_data::hudpacks::catalog::Snapshot,
    pub catalog_date: String,
    /// Stable stays off until the signed-index PR14.
    pub gallery_enabled: bool,
    pub app_version: String,
    pub language_tag: String,
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
    /// Any package operation, including a held offer and a confirmed write, excludes removal.
    pub package_busy: bool,
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
    Browse,
    Catalog(Sha),
    Preview,
    Use,
    InstallRegistry(Sha),
    About,
    Fact(u32),
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
    FetchRegistry(Sha),
    Use(String),
    RemoveAndReturn(String),
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
    kind: u8,
    scene: u32,
    visible: bool,
    suspended: bool,
    pending_select_focus: bool,
    registry_request: bool,
    provisional: bool,
}
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.u32(self.selected).u32(self.rows).u8(self.notice).bool(self.confirming).bool(self.affirmative).u32(self.alert_scroll)
            .bool(self.editing).bool(self.fetching).u8(self.kind).u32(self.scene).bool(self.visible).bool(self.suspended).bool(self.pending_select_focus).bool(self.registry_request).bool(self.provisional);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "player-hud sel={} rows={} confirm={} editing={} fetching={} view={} scene={}",
            self.selected, self.rows, self.confirming, self.editing, self.fetching, self.kind, self.scene
        ));
    }
}

pub struct HudPage {
    entry: EntryId,
    page: SettingsPage,
    title: String,
    parent: String,
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
    strip: plx_ui::widgets::TabStrip,
    controls: Option<plx_ui::preview_controls::PreviewControls>,
    preview_cycle: plx_ui::widgets::ArtCycle,
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
        Unloadable::Revoked => plx_platform::i18n::msg::settings_hud_revoked(),
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
        Err(Unloadable::Revoked) => facts.push(list.catalog.revoked(entry.sha.unwrap_or_else(|| Sha::of(&[])))
            .map_or_else(|| unloadable_text(Unloadable::Revoked).to_owned(), |why| format!("{} {why}", unloadable_text(Unloadable::Revoked)))),
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
fn hud_form(list: &HudList, draft: Option<&TextBuffer>, browse: Option<SettingsPage>) -> Form<HudId, HudAction, SettingsPage> {
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
        let managed_in_gallery = list.gallery_enabled && entry.sha.is_some_and(|sha|
            list.catalog.index.as_ref().is_some_and(|index| index.entries.iter().any(|p| p.sha() == sha
                && p.compatible(&list.app_version, &list.language_tag, plx_appkit::hud_format::bundle::MAX_ARTIFACT_BYTES)))
                && list.catalog.revoked(sha).is_none() && !list.unfit.contains(&sha));
        if entry.id != BUILTIN_CLASSIC && !managed_in_gallery {
            let row = Row::new(&name).server_label().detail(plx_platform::i18n::msg::settings_hud_remove());
            removals = removals.item_keyed(HudId::Remove(entry.id.clone()), RowKey(REMOVE_KEY + i as u32), RowKind::Button, HudAction::Remove(entry.id.clone()), row).disabled(list.package_busy);
        }
    }
    // The address row: the keyboard's text while it is up, "Downloading…" while a fetch runs, else what to type.
    let detail = match draft {
        Some(text) => text.with_insertion_marker("|"),
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
    let mut gallery = FormSection::new("");
    if let Some(page) = browse {
        gallery = gallery.item_keyed(HudId::Browse, RowKey(0x4000), RowKind::Nav(page), HudAction::Inert,
            Row::new(plx_platform::i18n::msg::settings_hud_browse()));
    }
    Form::new().section(packs).section(gallery).section(removals).section(address).section(folder)
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
            page: SettingsPage::PlayerHud,
            title: String::new(),
            parent: String::new(),
            form: { let mut form = FormTable::new(BAND); form.table.set_empty_text(Some("")); form },
            state: State { selected: 0, rows: 0, notice: 0, confirming: false, affirmative: false, alert_scroll: 0, editing: false, fetching: false, kind: 0, scene: 0, visible: false, suspended: false, pending_select_focus: false, registry_request: false, provisional: true },
            list: HudList::default(),
            copy: String::new(),
            rx,
            tx: Some(tx),
            asking: None,
            offered: None,
            draft: TextBuffer::new(String::new(), 0),
            address_error: None,
            strip: plx_ui::widgets::TabStrip::new(),
            controls: None,
            preview_cycle: plx_ui::widgets::ArtCycle::default(),
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

    pub fn at(entry: EntryId, page: SettingsPage) -> Self {
        let mut out = Self::new(entry);
        out.page = page;
        out.state.kind = match page { SettingsPage::HudGallery => 1, SettingsPage::HudDetail(..) => 2,
            SettingsPage::HudAbout(..) => 3, SettingsPage::HudPreview(..) => 4, _ => 0 };
        out.rebuild(None);
        out
    }

    fn package(&self) -> Option<&plx_data::hudpacks::catalog::Package> {
        let sha = match self.page {
            SettingsPage::HudDetail(sha) | SettingsPage::HudAbout(sha) | SettingsPage::HudPreview(sha) => sha,
            _ => return None,
        };
        self.list.catalog.package(sha)
    }

    fn gallery_form(&self) -> (Form<HudId, HudAction, SettingsPage>, String, String, String) {
        // placeholder-exempt: Selects copy only; Header::with_pending_copy counts and sentinel-paints its loading read-out.
        use plx_platform::i18n::msg;
        let mut rows = FormSection::new("");
        let (title, parent, mut copy) = match self.page {
            SettingsPage::PlayerHud => {
                let browse = self.list.gallery_enabled.then_some(SettingsPage::HudGallery);
                let form = hud_form(&self.list, self.state.editing.then_some(&self.draft), browse);
                return (form, msg::settings_hud_title().into(), parent_title().into(), copy_of(&self.list, self.address_error));
            }
            SettingsPage::HudGallery => {
                if self.list.gallery_enabled {
                    if let Some(index) = &self.list.catalog.index {
                        for (i, package) in index.entries.iter().enumerate().filter(|(_, p)|
                            p.compatible(&self.list.app_version, &self.list.language_tag, plx_appkit::hud_format::bundle::MAX_ARTIFACT_BYTES)
                                && self.list.catalog.revoked(p.sha()).is_none() && !self.list.unfit.contains(&p.sha())) {
                            rows = rows.item_keyed(HudId::Catalog(package.sha()), RowKey(i as u32),
                                RowKind::Nav(SettingsPage::HudDetail(package.sha())), HudAction::Inert,
                                Row::new(&package.name).server_label());
                        }
                    }
                }
                let empty = self.list.catalog.index.as_ref().is_none_or(|index| !index.entries.iter().any(|p|
                    p.compatible(&self.list.app_version, &self.list.language_tag, plx_appkit::hud_format::bundle::MAX_ARTIFACT_BYTES)
                        && self.list.catalog.revoked(p.sha()).is_none() && !self.list.unfit.contains(&p.sha())));
                let copy = if self.list.catalog.refresh_failed { if self.list.catalog.index.is_some() { msg::settings_hud_catalog_cached() } else { msg::settings_hud_catalog_unavailable() } }
                    else if self.list.catalog.fetching && empty { msg::settings_hud_loading() }
                    else if empty { msg::settings_hud_empty() } else { msg::settings_hud_gallery_copy() };
                (msg::settings_hud_gallery().into(), msg::settings_hud_title().into(), copy.into())
            }
            SettingsPage::HudDetail(sha) | SettingsPage::HudPreview(sha) => {
                let Some(package) = self.package() else {
                    return (Form::new(), msg::settings_hud_title().into(), msg::settings_hud_gallery().into(), msg::settings_hud_empty().into());
                };
                rows = rows.item_keyed(HudId::Preview, RowKey(0), RowKind::Nav(SettingsPage::HudPreview(sha)),
                    HudAction::Inert, Row::new(msg::settings_hud_preview()));
                let installed = self.list.entries.iter().find(|e| e.id == package.id);
                if let Some(entry) = installed.filter(|e| e.sha == Some(sha)) {
                    rows = rows.item_keyed(HudId::Use, RowKey(1), RowKind::Button, HudAction::Use(package.id.clone()),
                        Row::new(msg::settings_hud_use()).checked(entry.selected).dim(entry.loadable.is_err()))
                        .disabled(entry.loadable.is_err());
                } else {
                    let newer = installed.and_then(|e| e.meta.as_ref()).is_none_or(|m|
                        plx_data::hudpacks::Semver::parse(&m.version) < plx_data::hudpacks::Semver::parse(&package.version));
                    let listed = self.list.catalog.index.as_ref().is_some_and(|index| index.entries.iter().any(|p| p.sha() == sha));
                    let can = listed && newer && !self.list.package_busy
                        && self.list.catalog.revoked(sha).is_none() && !self.list.unfit.contains(&sha)
                        && package.compatible(&self.list.app_version, &self.list.language_tag, plx_appkit::hud_format::bundle::MAX_ARTIFACT_BYTES);
                    let label = if self.list.fetching { msg::settings_hud_fetching() }
                        else if installed.is_some() && newer { msg::settings_hud_update() } else { msg::settings_hud_install() };
                    let row = Row::new(label).dim(!can);
                    rows = rows.item_keyed(HudId::InstallRegistry(sha), RowKey(1), RowKind::Button, HudAction::FetchRegistry(sha), row).disabled(!can);
                }
                rows = rows.item_keyed(HudId::About, RowKey(2), RowKind::Nav(SettingsPage::HudAbout(sha)),
                    HudAction::Inert, Row::new(msg::settings_hud_about()));
                let listed = self.list.catalog.index.as_ref().is_some_and(|index| index.entries.iter().any(|p| p.sha() == sha));
                let copy = if !listed && self.list.catalog.revoked(sha).is_none() { msg::settings_hud_unavailable().into() }
                    else if let Some(why) = self.list.catalog.revoked(sha) { format!("{} {why}", msg::settings_hud_revoked()) }
                    else if self.list.catalog.refresh_failed && installed.is_none() { msg::settings_hud_catalog_cached().into() }
                    else { msg::settings_hud_copy().into() };
                (package.name.clone(), msg::settings_hud_gallery().into(), copy)
            }
            SettingsPage::HudAbout(_) => {
                let Some(package) = self.package() else {
                    return (Form::new(), msg::settings_hud_about().into(), msg::settings_hud_gallery().into(), msg::settings_hud_empty().into());
                };
                for (i, (label, value)) in [(msg::settings_hud_author(), package.author.clone()),
                    (msg::settings_hud_version(), package.version.clone()), (msg::settings_hud_license(), package.license.clone()),
                    (msg::settings_hud_download(), format!("{} B", plx_platform::i18n::current().number(package.bytes as i64)))].into_iter().enumerate() {
                    rows = rows.item_keyed(HudId::Fact(i as u32), RowKey(i as u32), RowKind::Button, HudAction::Inert,
                        Row::new(label).value(&value));
                }
                if !self.list.catalog_date.is_empty() { rows = rows.note(msg::settings_hud_cached(&self.list.catalog_date)); }
                if self.list.entries.iter().any(|e| e.id == package.id) {
                    rows = rows.item_keyed(HudId::Remove(package.id.clone()), RowKey(4), RowKind::Button,
                        HudAction::RemoveAndReturn(package.id.clone()), Row::new(msg::settings_hud_remove())).disabled(self.list.package_busy);
                }
                (msg::settings_hud_about().into(), package.name.clone(), String::new())
            }
            _ => unreachable!("HUD page only mounts its family"),
        };
        if matches!(self.page, SettingsPage::HudGallery) && self.list.catalog.refresh_failed && !self.list.catalog_date.is_empty() {
            copy = format!("{}\n\n{}", copy, msg::settings_hud_cached(&self.list.catalog_date));
        }
        if self.state.registry_request && matches!(self.list.notice, Some(Notice::FetchFailed {..} | Notice::Refused {..} | Notice::NotWritten {..})) {
            copy = copy_of(&self.list, None);
        }
        (Form::new().section(rows), title, parent, copy)
    }

    /// The focused catalog appearance. Its artwork is display-only; Preview is a separate row.
    fn preview_package(&self) -> Option<&plx_data::hudpacks::catalog::Package> {
        if let Some(package) = self.package() { return Some(package) }
        let HudId::Catalog(sha) = self.form.selected_id()? else { return None };
        self.list.catalog.index.as_ref()?.entries.iter().find(|p| p.sha() == *sha)
    }

    fn is_preview(&self) -> bool { matches!(self.page, SettingsPage::HudPreview(..)) }

    fn activate_preview(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) -> Handled {
        if key == 0 {
            fx.push(Fx::Nav(NavOp::Pop));
        } else {
            let at = key as usize - 1;
            if at < self.scenes().len() {
                self.state.scene = at as u32;
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
        }
        Handled::Yes
    }

    fn scenes(&self) -> Vec<(&'static str, &plx_data::hudpacks::catalog::Preview)> {
        use plx_platform::i18n::msg;
        let Some(package) = self.package() else { return Vec::new() };
        let mut scenes = Vec::new();
        if let Some(art) = package.preview(&self.list.language_tag) { scenes.push((msg::settings_hud_compact(), art)); }
        for (label, state) in [(msg::settings_hud_expanded(), "sections-3-movie-paused"), (msg::settings_hud_up_next(), "up-next")] {
            if let Some(art) = package.previews.iter().find(|p| p.state == state && p.lang == self.list.language_tag)
                .or_else(|| package.previews.iter().find(|p| p.state == state && p.lang == "en")) { scenes.push((label, art)); }
        }
        scenes
    }

    fn preview_active(&self) -> bool {
        self.state.visible && !self.state.suspended && !self.state.confirming
            && !matches!(self.page, SettingsPage::HudAbout(..))
    }

    /// Frames belong to one scene AND one resolved language. Cycling never changes a tab,
    /// metadata, package, or translation, and identical content hashes collapse in ArtCycle.
    fn preview_frames(&self) -> Vec<&str> {
        let primary = if self.is_preview() {
            self.scenes().get(self.state.scene as usize).map(|(_, art)| *art)
        } else {
            self.preview_package().and_then(|p| p.preview(&self.list.language_tag))
        };
        let Some(primary) = primary else { return Vec::new() };
        let Some(package) = self.preview_package() else { return Vec::new() };
        package.previews.iter().filter(|p| p.state == primary.state && p.lang == primary.lang)
            .map(|p| p.url.as_str()).collect()
    }

    fn view(&self) -> TableScreen<'_> {
        let mut header = Header::new(RouteLayout::screen(), Some(&self.parent), &self.title, &self.copy);
        if matches!(self.page, SettingsPage::HudGallery) && self.list.catalog.fetching && self.form.table.n_rows() == 0 {
            header = header.with_pending_copy("hud-gallery");
        }
        if !self.preview_cycle.is_empty() && !matches!(self.page, SettingsPage::HudAbout(..)) {
            header = header.with_preview_cycle(&self.preview_cycle);
        } else if !matches!(self.page, SettingsPage::HudAbout(..)) {
            if let Some(art) = self.preview_package().and_then(|p| p.preview(&self.list.language_tag)) {
                header = header.with_preview(plx_ui::widgets::Art::content_png(&art.url));
            }
        }
        TableScreen::new(header, &self.form.table, GroupId(0), self.entry).keyed(&self.form)
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
        let (form, title, parent, copy) = self.gallery_form();
        self.copy = copy;
        self.title = title;
        self.parent = parent;
        self.form.table.compact = false;
        self.form.table.header_ink = theme::TEXT_READING;
        self.form.table.list_focused = true;
        self.form.set(form, keep.as_ref());
        self.state.fetching = self.list.fetching;
        self.state.rows = self.form.table.n_rows() as u32;
        self.state.notice = match &self.list.notice {
            None => 0,
            Some(Notice::Installed { .. }) => 1,
            Some(Notice::Refused { .. }) => 2,
            Some(Notice::NotWritten { .. }) => 3,
            Some(Notice::Removed { .. }) => 4,
            Some(Notice::RemoveIncomplete { .. }) => 5,
            Some(Notice::FetchFailed { .. }) => 7,
        };
        self.state.selected = self.form.key_at(self.form.table.sel.max(0) as usize).map_or(0, |k| k.0);
    }

    /// Open the page's channel on the first event, so a page that never gets one asks for nothing.
    fn open(&mut self, fx: &mut Effects<'_, InnerHost>) {
        if let Some(tx) = self.tx.take() {
            let command = if self.page == SettingsPage::PlayerHud { HudCmd::Open(tx) } else { HudCmd::Watch(tx) };
            fx.push(Fx::App(AppFx::Huds(command)));
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
        if !self.state.visible { return }
        let Some(offer) = self.list.offer.clone() else {
            self.offered = None;
            return;
        };
        let owns = if offer.source == plx_data::hudpacks::Source::Registry {
            matches!(self.page, SettingsPage::HudDetail(sha) if sha == offer.sha)
        } else { self.page == SettingsPage::PlayerHud };
        if !owns { return }
        if self.offered == Some(offer.sha) {
            return;
        }
        self.offered = Some(offer.sha);
        if offer.source == plx_data::hudpacks::Source::Registry {
            self.ask(Asking::Offer(offer.sha), plx_platform::i18n::msg::settings_hud_gallery_install_question_c(),
                vec![Cow::Borrowed(plx_platform::i18n::msg::settings_hud_gallery_install_body())], fx);
            return;
        }
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
        self.state.provisional = false;
        match form_activate(&self.form, key, fx) {
            Some(HudAction::FetchRegistry(sha)) => {
                self.state.registry_request = true;
                fx.push(Fx::App(AppFx::Huds(HudCmd::FetchRegistry(sha))));
            }
            Some(HudAction::RemoveAndReturn(id)) => {
                fx.push(Fx::App(AppFx::Huds(HudCmd::Remove(id))));
                fx.push(Fx::Nav(NavOp::PopToArg(SettingsPage::PlayerHud)));
            }
            Some(HudAction::Use(id)) => {
                fx.push(Fx::App(AppFx::Huds(HudCmd::Select(id))));
                fx.push(Fx::Nav(NavOp::PopToArg(SettingsPage::PlayerHud)));
            }
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
        if self.is_preview() {
            match ev {
                ScreenEvent::FocusMoved { to, .. } => { self.state.selected = to.elem; return Handled::Yes }
                ScreenEvent::Activate(key) => return self.activate_preview(*key, fx),
                ScreenEvent::PressCommit(_) => return match cx.focus.current.filter(|key| key.entry == self.entry) {
                    Some(key) => self.activate_preview(key.elem, fx),
                    None => Handled::No,
                },
                ScreenEvent::Input(_) => return Handled::No,
                _ => (),
            }
        }
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
            ScreenEvent::Enter(enter) => {
                if matches!(enter, Enter::Restored) { self.state.provisional = false; }
                self.state.visible = true;
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
                    let before = self.list.entries.iter().find(|e| e.selected).map(|e| e.id.as_str());
                    let after = list.entries.iter().find(|e| e.selected).map(|e| e.id.as_str());
                    let picked = (self.page == SettingsPage::PlayerHud && ((before.is_some() && (before != after || matches!(list.notice, Some(Notice::Removed {..}))))
                        || (self.state.provisional && after.is_some())))
                        .then(|| after.map(str::to_owned)).flatten();
                    let scene = self.scenes().get(self.state.scene as usize).map(|(_, art)| art.state.clone());
                    self.list = list;
                    self.state.scene = scene.and_then(|state| self.scenes().iter().position(|(_, art)| art.state == state)).unwrap_or(0) as u32;
                    self.controls = None;
                    let keep = if let Some(id) = picked {
                        self.state.provisional = false;
                        self.state.pending_select_focus = true;
                        Some(HudId::Pack(id))
                    } else if matches!(self.page, SettingsPage::HudDetail(sha) if matches!(self.list.notice, Some(Notice::Installed {sha: installed, ..}) if installed == sha)) {
                        Some(HudId::Use)
                    } else { self.form.selected_id().cloned() };
                    self.rebuild(keep);
                    if !had_rows && self.form.table.n_rows() > 0 && cx.focus.current.is_none() {
                        self.focus(fx, GroupId(0));
                    }
                    fx.invalidate(plx_machine::present::Provenance::Landing(MachineId::Session));
                }
                // Run after the surface has finished restoring its remembered cursor on a pop.
                if self.state.visible && self.state.pending_select_focus {
                    self.state.pending_select_focus = false;
                    self.focus(fx, GroupId(0));
                }
                self.sync_offer(fx);
                if self.is_preview() {
                    if self.controls.is_none() {
                        self.controls = Some(plx_ui::preview_controls::PreviewControls::new(self.scenes().iter().map(|(label,_)| (*label).into()), cx.measure));
                    }
                    if let Some(controls) = &mut self.controls {
                        controls.update(&mut self.strip, self.state.scene as usize, cx.focus.current.map(|k| k.elem), t.dt());
                        if cx.focus.current.is_none() {
                            self.state.selected = controls.seat(self.entry, self.state.scene as usize).elem;
                            self.focus(fx, GroupId(0));
                        }
                    }
                }
                let mut cycle = std::mem::take(&mut self.preview_cycle);
                cycle.set_frames(&self.preview_frames());
                cycle.tick(*t, self.preview_active(), &mut fx.present());
                self.preview_cycle = cycle;
                self.form.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind, .. }) if self.state.editing || matches!(kind, InputKind::SystemKeyboard(_)) => self.typing(kind, cx, fx),
            ScreenEvent::Suspend => {
                self.preview_cycle.pause(cx.tick);
                self.state.suspended = true;
                self.leave_field(fx);
                Handled::No
            }
            ScreenEvent::Resume => {
                self.state.suspended = false;
                Handled::No
            }
            ScreenEvent::WillLeave(_) | ScreenEvent::Unmount | ScreenEvent::Cover => {
                self.preview_cycle.pause(cx.tick);
                self.state.visible = false;
                if matches!(ev, ScreenEvent::Unmount) {
                    if let SettingsPage::HudDetail(sha) = self.page { fx.push(Fx::App(AppFx::Huds(HudCmd::CancelRegistry(sha)))); }
                }
                self.leave_field(fx);
                Handled::No
            }
            ScreenEvent::FocusMoved { to, by, .. } => {
                if matches!(by, plx_machine::machine::By::Dir | plx_machine::machine::By::Pointer) { self.state.provisional = false; }
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
        if self.is_preview() { if let Some(controls) = &self.controls { out.push(controls.group(GroupId(0))); } return; }
        if !self.prompt.groups(out, cx.measure) {
            Focusable::<InnerHost>::groups(&self.view(), cx, out)
        }
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, InnerHost>) -> Option<GroupId> {
        if self.is_preview() { return self.controls.as_ref().filter(|c| c.contains(*key)).map(|_| GroupId(0)); }
        match self.prompt.group_of(*key) {
            Some(answer) => answer,
            None => Focusable::<InnerHost>::group_of(&self.view(), key, cx),
        }
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, InnerHost>) -> Step<u32> {
        if self.is_preview() { return self.controls.as_ref().map_or(Step::Edge, |c| c.neighbour(key, dir)); }
        match self.prompt.neighbour(key, dir) {
            Some(step) => step,
            None => Focusable::<InnerHost>::neighbour(&self.view(), key, dir, cx),
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, InnerHost>, at: At) -> Option<Placed> {
        if self.is_preview() { return self.controls.as_ref()?.place(*key); }
        match self.prompt.place(*key, cx.measure) {
            Some(placed) => placed,
            None => Focusable::<InnerHost>::place(&self.view(), key, cx, at),
        }
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        if self.is_preview() { return self.controls.as_ref().map_or(want, |c| c.reconcile(want, self.state.scene as usize)); }
        if let Some(key) = self.prompt.reconcile(want) {
            return key;
        }
        if self.prompt.owns(want.elem) {
            return FocusKey { entry: self.entry, elem: self.state.selected };
        }
        Focusable::<InnerHost>::reconcile(&self.view(), want, cx)
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        if self.is_preview() { return self.controls.as_ref().map_or(FocusKey {entry:self.entry,elem:0}, |c| c.seat(self.entry, self.state.scene as usize)); }
        match self.prompt.seat(g, self.entry) {
            Some(key) => key,
            None => Focusable::<InnerHost>::seat(&self.view(), g, from, cx),
        }
    }
}

impl Screen<InnerHost> for HudPage {
    fn name(&self) -> &'static str {
        match self.page {
            SettingsPage::HudGallery => word::HUD_GALLERY,
            SettingsPage::HudDetail(..) => word::HUD_DETAIL,
            SettingsPage::HudAbout(..) => word::HUD_ABOUT,
            SettingsPage::HudPreview(..) => word::HUD_PREVIEW,
            _ => word::PLAYER_HUD,
        }
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(&self.parent))
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        if self.is_preview() {
            let p = f.painter.alpha(f.page_alpha);
            if self.preview_cycle.is_empty() {
                if let Some((_, art)) = self.scenes().get(self.state.scene as usize) {
                    plx_ui::widgets::card(p, Rect::FULL, plx_ui::widgets::Art::content_png(&art.url), 0.0, false, 1.0, 0.0);
                }
            } else {
                self.preview_cycle.draw(p, Rect::FULL, 0.0);
            }
            if let Some(controls) = &mut self.controls {
                controls.draw(p, &self.strip, f.cx.focus.current.map(|k| k.elem));
                for elem in 0..controls.len() as u32 {
                    if let Some(at) = controls.place(elem) {
                        f.stop(p, plx_ui::screen::Stop { key: FocusKey { entry: self.entry, elem }, rect: at.rect,
                            rest_rect: at.rest_rect, clip: at.clip, hover: plx_ui::screen::Hover::Focus, activate: plx_ui::screen::Activate::Press });
                    }
                }
            }
        } else { plx_ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL); }
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
    form.set(hud_form(list, None, None), None);
    (0..form.table.n_rows() as usize).filter_map(|i| Some((form.key_at(i)?.0, form.binding_at(i)?.id.clone()))).collect()
}

#[cfg(test)]
#[path = "hud_settings_tests.rs"]
mod tests;
