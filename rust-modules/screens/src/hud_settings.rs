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
//! **Fetch a development source again** (PR15, `tools/hud-dev`). A package installed from a development source (an
//! `http://` address on the home network: an author's machine) is listed with that address ([`Entry::dev_source`]).
//! Play on its row sends the same `HudCmd::Fetch` the address row sends, so the author sees the next build through
//! the same offer, prompt and confirmation, and nothing is installed without the OK. It is the remote's own ▶ read by
//! the shared classifier ([`consts::classify_input`]), taken on a fresh press only and only while the page is free
//! (no fetch or package work in flight, no keyboard or prompt up). On any other row Play is left unhandled; no row
//! and no string is added.
//!
//! **What a row says** (Settings v2). A row is its label and, when it cannot be used, the reason as its value
//! ([`Row::disabled`]: the ghost focus pill, `rust-modules/ui/src/CLAUDE.md` rule 5, the disabled paragraph): the package's `name` (the author's text: a
//! server-style label, never measured by the fit report; the app's own HUD is named *Built-in*, which is also its
//! tier, so its row says no tier under it), *Unreviewed* under it for a sideload (a status, so it keeps its sub-line),
//! and, for a package that cannot run, why as the value: a format the app does not read, an app range it is outside
//! of, a withdrawal, or "Doesn't fit in {language}" (a compile the television refused in the current language, where
//! classic draws and the selection is kept; the row is DISABLED, so OK on it does nothing). "Installed until restart", when only the volatile directory holds a
//! usable package, joins the sub-line. Everything else a row used to say under its name is the **sentence that follows
//! focus** in the left column ([`FollowCopy`]; [`HudPage::sentence`] is a pure function of the focused row and the
//! listing): the page's own while a row has none, a refusal or fetch notice outranking every row's.
use std::borrow::Cow;
use std::sync::mpsc::{self, Receiver};

use plx_data::hudpacks::address::{self, AddressError};
use plx_data::hudpacks::{DropFile, Entry, FetchError, Notice, OfferView, Refusal, Sha, Trust, Unloadable, BUILTIN_CLASSIC};
use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, InputOwner, InstanceId, Key, LogicalState,
    Machine, MachineId, TextEdit, NavOp,
};
use plx_ui::decision_prompt::{DecisionPrompt, PromptStep};
use plx_ui::follow_copy::{FollowCopy, LandscapePicture};
use plx_ui::text_buffer::TextBuffer;
use plx_ui::form::{Form, FormSection, FormTable, RowKey, RowKind};
use plx_ui::frame::Budget;
use plx_ui::route_screen::RouteLayout;
use plx_ui::screen::{At, Dir, DrawFrame, Enter, Focusable, FocusSource, FocusTarget, GroupSpec, HitSource, Placed, RenderStrategy, Screen, ScreenEvent, Step};
use plx_ui::table::Row;
use plx_ui::table_screen::{Header, TableScreen};
use plx_ui::{consts, theme, Rect};

use super::family::{form_activate, form_focus, form_right_target, InnerHost, SettingsPage, ALERT_GROUP};
use super::registry::{word, AppFx, HudCmd, ALERT, BAND};

/// What the Bridge publishes to the page: the store's listing at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HudList {
    pub catalog: plx_data::hudpacks::catalog::Snapshot,
    pub catalog_date: String,
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
    /// The sentence each row with one of its own says beside the table while it has focus ([`HudPage::sentence`]).
    says: Says,
    /// The sentence beside the table, which follows the focused row. Decoration: not part of the canonical state.
    follow: FollowCopy,
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

/// The package row's name: the built-in is "Built-in" (the app's own word for itself, and the one string its row says),
/// any other the author's text.
fn name_of(entry: &Entry) -> String {
    match &entry.meta {
        Some(meta) => meta.name.clone(),
        None => plx_platform::i18n::msg::settings_hud_builtin().to_owned(),
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

/// Why a package cannot be used now. The reason is its row's value ([`why_value`]); the sentence under the title says
/// what to do ([`why_sentence`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Why {
    /// The app will not run it: a format or app range it does not accept, or a withdrawal.
    Format(Unloadable),
    /// The television refused its compile in the current language, so classic draws and the selection is kept.
    Language,
}

/// Why `entry` cannot be used, if it cannot.
fn why_of(entry: &Entry, list: &HudList) -> Option<Why> {
    match entry.loadable {
        Err(why) => Some(Why::Format(why)),
        Ok(()) if entry.sha.is_some_and(|sha| list.unfit.contains(&sha)) => Some(Why::Language),
        Ok(()) => None,
    }
}

/// The reason, as the row's value: short, in the viewer's words.
fn why_value(why: Why, list: &HudList) -> String {
    match why {
        Why::Format(why) => unloadable_text(why).to_owned(),
        Why::Language => plx_platform::i18n::msg::settings_hud_unfit_language(&list.language),
    }
}

/// What the line under the title says while the row of a HUD that cannot be used has focus: why, and what to do. A
/// withdrawn HUD adds the catalog's own reason when it gave one.
fn why_sentence(why: Why, list: &HudList, sha: Option<Sha>) -> String {
    use plx_platform::i18n::msg;
    match why {
        Why::Format(Unloadable::Revoked) => {
            let said = msg::settings_hud_withdrawn_detail();
            match list.catalog.revoked(sha.unwrap_or_else(|| Sha::of(&[]))) {
                Some(reason) => format!("{said} {reason}"),
                None => said.to_owned(),
            }
        }
        Why::Format(Unloadable::NewerFormat) => msg::settings_hud_newer_detail().to_owned(),
        Why::Format(Unloadable::OlderFormat) => msg::settings_hud_older_detail().to_owned(),
        Why::Format(Unloadable::AppRange) => msg::settings_hud_app_range_detail().to_owned(),
        Why::Language => msg::settings_hud_unfit_detail(&list.language),
    }
}

/// `row` as the row of a HUD that has `why` (or none): the reason as its value. The ghost state, and OK doing nothing,
/// are the section's `.disabled(why.is_some())`: one mechanism ([`plx_ui::form::FormSection::disabled`]).
fn with_why(row: Row, why: Option<Why>, list: &HudList) -> Row {
    match why {
        Some(why) => row.value(why_value(why, list)),
        None => row,
    }
}

/// What a package row says under its name: its trust tier (*Unreviewed* for a sideload; a registry package says
/// none, and the built-in's tier is its name, [`name_of`], so it is not said twice) and, when the package is usable,
/// "Installed until restart" when only the volatile directory holds it. Why a package cannot be used is its row's
/// value ([`why_of`]), not a fact here.
fn facts_of(entry: &Entry, list: &HudList) -> Vec<String> {
    let mut facts = Vec::new();
    match entry.trust {
        Trust::BuiltIn | Trust::Registry => {}
        Trust::Unreviewed => facts.push(plx_platform::i18n::msg::settings_hud_unreviewed().to_owned()),
    }
    // The reason a package cannot be used outranks "installed until restart": the value column and a second fact do
    // not both fit a row in every language.
    if entry.until_restart && why_of(entry, list).is_none() {
        facts.push(plx_platform::i18n::msg::settings_hud_until_restart().to_owned());
    }
    facts
}

/// One row's sentence: the row, and what the line under the title says while it has focus.
type Says = Vec<(HudId, String)>;

/// The page's rows from the list alone: the packages (the built-in first), a removal row for each installed
/// one, and the drop files under their own header when there are any. Beside them, the sentence each row that has
/// one of its own says while it has focus.
fn hud_form(list: &HudList, draft: Option<&TextBuffer>) -> (Form<HudId, HudAction, SettingsPage>, Says) {
    use plx_platform::i18n::msg;
    let mut says = Says::new();
    let mut packs = FormSection::new("");
    let mut removals = FormSection::new("");
    for (i, entry) in list.entries.iter().enumerate() {
        let name = name_of(entry);
        let why = why_of(entry, list);
        let mut row = with_why(Row::new(&name).checked(entry.selected).detail(join(&facts_of(entry, list))), why, list);
        if entry.meta.is_some() {
            row = row.server_label();
        }
        packs = packs.item_keyed(HudId::Pack(entry.id.clone()), RowKey(i as u32), RowKind::Choice, HudAction::Select(entry.id.clone()), row)
            .disabled(why.is_some());
        if let Some(why) = why {
            says.push((HudId::Pack(entry.id.clone()), why_sentence(why, list, entry.sha)));
        }
        let managed_in_gallery = entry.sha.is_some_and(|sha|
            list.catalog.index.as_ref().is_some_and(|index| index.entries.iter().any(|p| p.sha() == sha
                && p.compatible(&list.app_version, &list.language_tag, plx_appkit::hud_format::bundle::MAX_ARTIFACT_BYTES)))
                && list.catalog.revoked(sha).is_none() && !list.unfit.contains(&sha));
        if entry.id != BUILTIN_CLASSIC && !managed_in_gallery {
            let row = Row::new(msg::settings_hud_remove_named(&name)).server_label();
            removals = removals.item_keyed(HudId::Remove(entry.id.clone()), RowKey(REMOVE_KEY + i as u32), RowKind::Button, HudAction::Remove(entry.id.clone()), row).disabled(list.package_busy);
            says.push((HudId::Remove(entry.id.clone()), msg::settings_hud_remove_detail().to_owned()));
        }
    }
    // The address row: the keyboard's text while it is up, "Downloading…" while a fetch runs, else what to type. The
    // line is a field hint (and the typed text), so it stays the row's sub-line.
    let detail = match draft {
        Some(text) => text.with_insertion_marker("|"),
        None if list.fetching => msg::settings_hud_fetching().to_owned(),
        None => msg::settings_hud_address_hint().to_owned(),
    };
    let row = Row::new(msg::settings_hud_address()).detail(detail).dim(list.fetching);
    let address = FormSection::new("").item_keyed(HudId::Address, RowKey(ADDRESS_KEY), RowKind::Button, HudAction::Address, row);
    let mut folder = FormSection::new(msg::settings_hud_folder()).visible(!list.drops.is_empty());
    for (i, file) in list.drops.iter().enumerate() {
        let name = file.meta.as_ref().map_or_else(|| file.name.clone(), |m| m.name.clone());
        // An installable file keeps its sub-line (what OK does, and that the file is unreviewed); one that cannot be
        // installed says why as its value, and the sentence says what to do.
        let mut row = Row::new(&name).server_label();
        let (id, action) = match file.sha {
            Some(sha) => (HudId::Drop(sha), HudAction::Install(sha)),
            None => (HudId::DropFile(i), HudAction::Inert),
        };
        match file.status {
            Ok(()) => row = row.detail(join(&[msg::settings_hud_install().to_owned(), msg::settings_hud_unreviewed().to_owned()])),
            Err(why) => {
                row = row.value(refusal_text(why));
                says.push((id.clone(), match why {
                    Refusal::Unloadable(why) => why_sentence(Why::Format(why), list, file.sha),
                    _ => msg::settings_hud_file_detail().to_owned(),
                }));
            }
        }
        folder = folder.item_keyed(id, RowKey(DROP_KEY + i as u32), RowKind::Button, action, row).disabled(file.status.is_err());
    }
    let gallery = FormSection::new("").item_keyed(HudId::Browse, RowKey(0x4000), RowKind::Nav(SettingsPage::HudGallery), HudAction::Inert,
        Row::new(msg::settings_hud_browse()));
    (Form::new().section(packs).section(gallery).section(removals).section(address).section(folder), says)
}

/// What a refusal, a failed fetch or an install no storage took puts under the title, while one is in force: the typed
/// address's refusal, else the last notice's. `None` when nothing is, and the page says its own sentence.
fn notice_text(list: &HudList, refused: Option<AddressError>) -> Option<String> {
    if let Some(why) = refused {
        return Some(address_text(why).to_owned());
    }
    match &list.notice {
        Some(Notice::Refused { reason, .. }) => Some(refusal_text(*reason).to_owned()),
        Some(Notice::FetchFailed { reason }) => Some(fetch_text(*reason).to_owned()),
        Some(Notice::NotWritten { .. }) => Some(plx_platform::i18n::msg::settings_hud_not_saved().to_owned()),
        _ => None,
    }
}

/// What the line under the title says: the last notice, else what the page is.
fn copy_of(list: &HudList, refused: Option<AddressError>) -> String {
    notice_text(list, refused).unwrap_or_else(|| plx_platform::i18n::msg::settings_hud_copy().to_owned())
}

/// What a page builds from the listing.
struct Built {
    form: Form<HudId, HudAction, SettingsPage>,
    says: Says,
    title: String,
    parent: String,
    /// The page's own sentence.
    copy: String,
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
            says: Says::new(),
            follow: FollowCopy::new(""),
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
        // a page opens on its first sentence at once, with nothing fading
        page.follow = FollowCopy::new(&page.sentence());
        page
    }

    pub fn at(entry: EntryId, page: SettingsPage) -> Self {
        let mut out = Self::new(entry);
        out.page = page;
        out.state.kind = match page { SettingsPage::HudGallery => 1, SettingsPage::HudDetail(..) => 2,
            SettingsPage::HudAbout(..) => 3, SettingsPage::HudPreview(..) => 4, _ => 0 };
        out.rebuild(None);
        out.follow = FollowCopy::new(&out.sentence());
        out
    }

    fn package(&self) -> Option<&plx_data::hudpacks::catalog::Package> {
        let sha = match self.page {
            SettingsPage::HudDetail(sha) | SettingsPage::HudAbout(sha) | SettingsPage::HudPreview(sha) => sha,
            _ => return None,
        };
        self.list.catalog.package(sha)
    }

    /// The page's rows and words from the listing alone.
    fn gallery_form(&self) -> Built {
        // placeholder-exempt: Selects copy only; Header::with_pending_copy counts and sentinel-paints its loading read-out.
        use plx_platform::i18n::msg;
        let mut rows = FormSection::new("");
        let mut says = Says::new();
        // the page's STATE (a delisted or withdrawn package, a catalog that could not refresh) is the
        // sentence beside the table whatever row has focus, as a refusal notice is on the Player HUD page
        let mut stateful = false;
        let (title, parent, mut copy) = match self.page {
            SettingsPage::PlayerHud => {
                let (form, says) = hud_form(&self.list, self.state.editing.then_some(&self.draft));
                return Built { form, says, title: msg::settings_hud_title().into(), parent: parent_title().into(), copy: copy_of(&self.list, self.address_error) };
            }
            SettingsPage::HudGallery => {
                if let Some(index) = &self.list.catalog.index {
                    for (i, package) in index.entries.iter().enumerate().filter(|(_, p)|
                        p.compatible(&self.list.app_version, &self.list.language_tag, plx_appkit::hud_format::bundle::MAX_ARTIFACT_BYTES)
                            && self.list.catalog.revoked(p.sha()).is_none() && !self.list.unfit.contains(&p.sha())) {
                        rows = rows.item_keyed(HudId::Catalog(package.sha()), RowKey(i as u32),
                            RowKind::Nav(SettingsPage::HudDetail(package.sha())), HudAction::Inert,
                            Row::new(&package.name).server_label());
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
                    return Built { form: Form::new(), says, title: msg::settings_hud_title().into(), parent: msg::settings_hud_gallery().into(), copy: msg::settings_hud_empty().into() };
                };
                rows = rows.item_keyed(HudId::Preview, RowKey(0), RowKind::Nav(SettingsPage::HudPreview(sha)),
                    HudAction::Inert, Row::new(msg::settings_hud_preview()));
                says.push((HudId::Preview, msg::settings_hud_preview_detail().into()));
                let installed = self.list.entries.iter().find(|e| e.id == package.id);
                if let Some(entry) = installed.filter(|e| e.sha == Some(sha)) {
                    let why = why_of(entry, &self.list);
                    rows = rows.item_keyed(HudId::Use, RowKey(1), RowKind::Button, HudAction::Use(package.id.clone()),
                        with_why(Row::new(msg::settings_hud_use()).checked(entry.selected), why, &self.list))
                        .disabled(why.is_some());
                    says.push((HudId::Use, why.map_or_else(|| msg::settings_hud_use_detail().to_owned(), |why| why_sentence(why, &self.list, Some(sha)))));
                } else {
                    let newer = installed.and_then(|e| e.meta.as_ref()).is_none_or(|m|
                        plx_data::hudpacks::Semver::parse(&m.version) < plx_data::hudpacks::Semver::parse(&package.version));
                    let listed = self.list.catalog.index.as_ref().is_some_and(|index| index.entries.iter().any(|p| p.sha() == sha));
                    // why this package cannot be installed, when the catalog or the language says so
                    let why = if self.list.catalog.revoked(sha).is_some() { Some(Why::Format(Unloadable::Revoked)) }
                        else if self.list.unfit.contains(&sha) { Some(Why::Language) }
                        else if !package.compatible(&self.list.app_version, &self.list.language_tag, plx_appkit::hud_format::bundle::MAX_ARTIFACT_BYTES) { Some(Why::Format(Unloadable::AppRange)) }
                        else { None };
                    let can = listed && newer && !self.list.package_busy && why.is_none();
                    // a disabled Install always says why: the format, language or app range, else the catalog no
                    // longer lists the package, else an equal or newer version is installed (busy has its own label)
                    let reason = why.map(|why| why_value(why, &self.list)).or_else(|| if !listed { Some(msg::settings_hud_unavailable_value().to_owned()) }
                        else if !newer { Some(msg::settings_hud_already().to_owned()) } else { None });
                    let updating = installed.is_some() && newer;
                    let label = if self.list.fetching { msg::settings_hud_fetching() }
                        else if updating { msg::settings_hud_update() } else { msg::settings_hud_install() };
                    let row = reason.iter().fold(Row::new(label), |row, reason| row.value(reason));
                    rows = rows.item_keyed(HudId::InstallRegistry(sha), RowKey(1), RowKind::Button, HudAction::FetchRegistry(sha), row).disabled(!can);
                    // no sentence of its own when its value already says why (delisted, or not newer): the page's
                    says.extend(match why {
                        Some(why) => Some(why_sentence(why, &self.list, Some(sha))),
                        None if reason.is_some() => None,
                        None if updating => Some(msg::settings_hud_update_detail().to_owned()),
                        None => Some(msg::settings_hud_install_detail().to_owned()),
                    }.map(|says| (HudId::InstallRegistry(sha), says)));
                }
                rows = rows.item_keyed(HudId::About, RowKey(2), RowKind::Nav(SettingsPage::HudAbout(sha)),
                    HudAction::Inert, Row::new(msg::settings_hud_about()));
                says.push((HudId::About, msg::settings_hud_about_detail().into()));
                let listed = self.list.catalog.index.as_ref().is_some_and(|index| index.entries.iter().any(|p| p.sha() == sha));
                let copy = if !listed && self.list.catalog.revoked(sha).is_none() { msg::settings_hud_unavailable().into() }
                    else if let Some(why) = self.list.catalog.revoked(sha) { format!("{} {why}", msg::settings_hud_revoked()) }
                    else if self.list.catalog.refresh_failed && installed.is_none() { msg::settings_hud_catalog_cached().into() }
                    else { msg::settings_hud_copy().into() };
                stateful = copy != msg::settings_hud_copy();
                (package.name.clone(), msg::settings_hud_gallery().into(), copy)
            }
            SettingsPage::HudAbout(_) => {
                let Some(package) = self.package() else {
                    return Built { form: Form::new(), says, title: msg::settings_hud_about().into(), parent: msg::settings_hud_gallery().into(), copy: msg::settings_hud_empty().into() };
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
                    says.push((HudId::Remove(package.id.clone()), msg::settings_hud_remove_detail().into()));
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
        if stateful { says.clear(); }
        Built { form: Form::new().section(rows), says, title, parent, copy }
    }

    /// **The sentence beside the table**, a pure function of the focused row and the listing: a refusal or fetch
    /// notice in force outranks everything; else the focused row's own ([`Says`]: why it cannot be used, or what it
    /// does); else the page's.
    fn sentence(&self) -> String {
        let noticed = match self.page {
            SettingsPage::PlayerHud => notice_text(&self.list, self.address_error).is_some(),
            _ => self.state.registry_request && matches!(self.list.notice, Some(Notice::FetchFailed {..} | Notice::Refused {..} | Notice::NotWritten {..})),
        };
        if noticed {
            return self.copy.clone();
        }
        self.form.selected_id()
            .and_then(|id| self.says.iter().find(|(row, _)| row == id))
            .map_or_else(|| self.copy.clone(), |(_, says)| says.clone())
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
        let header = Header::new(RouteLayout::settings_page(), Some(&self.parent), &self.title, &self.copy);
        // The gallery's loading read-out is the page's own copy, counted and sentinel-painted as one; any other
        // sentence follows focus.
        let header = if matches!(self.page, SettingsPage::HudGallery) && self.list.catalog.fetching && self.form.table.n_rows() == 0 {
            header.with_pending_copy("hud-gallery")
        } else {
            header.with_follow(&self.follow)
        };
        TableScreen::new(header, &self.form.table, GroupId(0), self.entry).keyed(&self.form)
    }

    /// The picture slot's art: the focused (gallery) or shown (package page) HUD's preview, display-only. Nothing on
    /// About, on a page whose focus is on no HUD, or for a HUD with no preview in the language.
    fn picture(&self) -> Option<LandscapePicture<'_>> {
        if matches!(self.page, SettingsPage::HudAbout(..)) {
            return None;
        }
        let art = self.preview_package().and_then(|p| p.preview(&self.list.language_tag))
            .map(|art| plx_ui::widgets::Art::content_png(&art.url));
        let cycle = (!self.preview_cycle.is_empty()).then_some(&self.preview_cycle);
        (art.is_some() || cycle.is_some()).then_some(LandscapePicture { art, cycle })
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
        let Built { form, says, title, parent, copy } = self.gallery_form();
        self.copy = copy;
        self.says = says;
        self.title = title;
        self.parent = parent;
        self.form.table.compact = false;
        self.form.table.board_metrics(true);
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
        self.follow.follow(&self.sentence());
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

    /// Play on the row of a package from a development source: its recorded address leaves as the fetch the address row
    /// sends. Unhandled on any other row, and while a fetch or other package work runs (an open prompt has already
    /// taken the key in `step`, before this arm).
    fn refetch(&mut self, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        if self.list.fetching || self.list.package_busy {
            return Handled::No;
        }
        let row = cx.focus.current.and_then(|key| self.form.index_of_key(RowKey(key.elem)));
        let Some(HudId::Pack(id)) = row.and_then(|i| self.form.id_at(i)) else { return Handled::No };
        let Some(address) = self.list.entries.iter().find(|entry| entry.id == *id).and_then(|entry| entry.dev_source.clone()) else {
            return Handled::No;
        };
        fx.push(Fx::App(AppFx::Huds(HudCmd::Fetch(address))));
        Handled::Yes
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
                    if !had_rows && self.form.table.n_rows() > 0 {
                        // the first rows are the page arriving, not a focus move: its sentence is there at once
                        self.follow = FollowCopy::new(&self.sentence());
                    }
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
                self.form.table.update(t.dt(), RouteLayout::settings_page().sectioned_table().h);
                self.follow.update(t.dt());
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
                self.follow.follow(&self.sentence());
                Handled::Yes
            }
            ScreenEvent::Activate(key) => {
                self.activate(*key, fx);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, sym, wcode, edge: Edge::Down, .. }, .. })
                if matches!(consts::classify_input(*key, *sym, *wcode), consts::Key::Play) => self.refetch(cx, fx),
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
        } else {
            // the picture slot, built from the focused row and the listing alone
            let picture = self.picture();
            let mut view = self.view();
            if let Some(picture) = &picture {
                view.header.picture = Some(picture);
            }
            plx_ui::screen::Part::<InnerHost>::draw(&mut view, f, Rect::FULL);
        }
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
    form.set(hud_form(list, None).0, None);
    (0..form.table.n_rows() as usize).filter_map(|i| Some((form.key_at(i)?.0, form.binding_at(i)?.id.clone()))).collect()
}

#[cfg(test)]
#[path = "hud_settings_tests.rs"]
mod tests;
