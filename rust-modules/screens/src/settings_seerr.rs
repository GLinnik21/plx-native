//! **Settings > Services > Seerr** (docs/seerr.md): the household's Seerr server (Overseerr,
//! Jellyseerr), which the request surfaces ask on behalf of whoever is watching.
//!
//! The rows: the server address (OK opens the television's keyboard and the address is typed in
//! the row itself, which stands above the raised keyboard), "Sign in with" (the seated profile's
//! Plex account, or a Seerr account of its own: OK switches), for a Seerr account its Email and
//! Password (typed in their rows like the address), Connect (Sign in for a Seerr account; its
//! sub-line is the live connection: connecting, connected as whom, refused, unreachable, signed
//! out) and Remove server. The page reads the stored address, the way the profile signs in and the
//! connection from the Seerr store's view (`cx.views.seerr`), never from the session: the store is
//! handed them every frame (`Bridge::seerr_pump`) and signs the seated profile in there.
//!
//! The address write goes through `PreferenceCmd::SeerrAddress` and the sign-in way through
//! `PreferenceCmd::SeerrMethod`: the rows show the new value at once, and the stored one settles it
//! when the receipt answers. Connect is `PreferenceCmd::SeerrConnect`; Sign in is
//! `PreferenceCmd::SeerrSignIn`, which carries the typed password away, and the page forgets it
//! then (it also forgets it when the page goes). The keyboard is raised and lowered with
//! `Delivery::Keyboard`, which the surface re-addresses to itself (it is the input owner the
//! dispatcher sees); the text and the panel's own close come back as input. BACK while typing puts
//! the field back; OK, the panel's close or leaving the row keeps what was typed.

use super::*;
use plx_data::stores::seerr::{Connection, SeerrAccount};
use plx_machine::machine::{Edge, InputEvent, InputKind, TextEdit};
use plx_platform::i18n::msg;
use plx_ui::text_buffer::TextBuffer;
use plx_ui::text_field::{self, FieldRun};

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum SeerrId {
    Address,
    Method,
    Email,
    Password,
    Connect,
    Remove,
}

impl FormId for SeerrId {
    fn key(&self) -> RowKey {
        RowKey(match self {
            SeerrId::Address => 1,
            SeerrId::Connect => 2,
            SeerrId::Method => 3,
            SeerrId::Email => 4,
            SeerrId::Password => 5,
            SeerrId::Remove => 30,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum SeerrAction {
    Address,
    Method,
    Email,
    Password,
    Connect,
    Remove,
}

/// The Settings root's value for the Seerr row: whether an address is set.
pub(super) fn summary(address: &str) -> &'static str {
    if address.trim().is_empty() { msg::settings_seerr_value_off() } else { msg::settings_seerr_value_on() }
}

/// The Connect row's sub-line: what the connection is.
pub(super) fn status_text(address: &str, connection: Option<&Connection>) -> String {
    if address.trim().is_empty() {
        return msg::settings_seerr_status_off().to_string();
    }
    match connection {
        Some(Connection::Connecting) => msg::settings_seerr_status_connecting().to_string(),
        Some(Connection::Connected { name }) => msg::settings_seerr_status_connected(name),
        Some(Connection::NotAllowed) => msg::settings_seerr_status_not_allowed().to_string(),
        Some(Connection::Unreachable) => msg::settings_seerr_status_unreachable().to_string(),
        Some(Connection::BadAddress) => msg::settings_seerr_status_bad_address().to_string(),
        Some(Connection::SignedOut) => msg::settings_seerr_status_signed_out().to_string(),
        Some(Connection::BadCredentials) => msg::settings_seerr_status_bad_credentials().to_string(),
        Some(Connection::Off | Connection::Idle) | None => msg::settings_seerr_status_idle().to_string(),
    }
}

/// The page's rows from plain inputs (the text-fit tests call this directly). `account` is the
/// Seerr account's email while the profile signs in with one, `None` for its Plex account;
/// `password` is whether a password is typed.
pub(super) fn seerr_form(address: &str, status: &str, account: Option<&str>, password: bool)
    -> Form<SeerrId, SeerrAction, SettingsPage>
{
    let set = !address.trim().is_empty();
    let (method, method_detail) = match account {
        Some(_) => (msg::settings_seerr_method_account(), msg::settings_seerr_method_detail_account()),
        None => (msg::settings_seerr_method_plex(), msg::settings_seerr_method_detail_plex()),
    };
    let mut rows = FormSection::new("")
        .item(
            SeerrId::Address,
            RowKind::Button,
            SeerrAction::Address,
            Row::new(msg::settings_seerr_address_title())
                .detail(msg::settings_seerr_address_detail())
                .value(if set { address.trim() } else { msg::settings_seerr_address_unset() }),
        )
        .item(
            SeerrId::Method,
            RowKind::Button,
            SeerrAction::Method,
            Row::new(msg::settings_seerr_method_title()).detail(method_detail).value(method).dim(!set),
        );
    if let Some(email) = account {
        rows = rows
            .item(
                SeerrId::Email,
                RowKind::Button,
                SeerrAction::Email,
                Row::new(msg::settings_seerr_email_title())
                    .detail(msg::settings_seerr_email_detail())
                    .value(if email.is_empty() { msg::settings_seerr_address_unset() } else { email })
                    .dim(!set),
            )
            .item(
                SeerrId::Password,
                RowKind::Button,
                SeerrAction::Password,
                Row::new(msg::settings_seerr_password_title())
                    .detail(msg::settings_seerr_password_detail())
                    .value(if password { msg::settings_seerr_password_set() } else { msg::settings_seerr_password_unset() })
                    .dim(!set),
            );
    }
    let (connect, ready) = match account {
        Some(email) => (msg::settings_seerr_sign_in_title(), set && !email.is_empty() && password),
        None => (msg::settings_seerr_connect_title(), set),
    };
    let rows = rows.item(
        SeerrId::Connect,
        RowKind::Button,
        SeerrAction::Connect,
        Row::new(connect).detail(status).dim(!ready),
    );
    let remove = FormSection::new("").visible(set).item(
        SeerrId::Remove,
        RowKind::Button,
        SeerrAction::Remove,
        Row::new(msg::settings_seerr_remove_title()).detail(msg::settings_seerr_remove_detail()),
    );
    Form::new().section(rows).section(remove)
}

pub(super) struct SeerrPage {
    entry: EntryId,
    form: FormTable<SeerrId, SeerrAction, SettingsPage>,
    state: SeerrState,
    /// While the keyboard is up: which field is typed into, and what it holds.
    draft: Option<(SeerrId, TextBuffer)>,
    /// An address written here and not yet answered: shown meanwhile, and the write's receipt.
    pending: Option<(String, std::sync::mpsc::Receiver<bool>)>,
    /// A sign-in way written here and not yet answered (`Some(email)` a Seerr account), likewise.
    method_pending: Option<(Option<String>, std::sync::mpsc::Receiver<bool>)>,
    /// The password typed for the next Sign in. Never written anywhere: Sign in carries it away,
    /// and leaving the page drops it.
    password: Option<String>,
    /// The Connect row's sub-line as last built, so a new connection rebuilds the rows once.
    status: String,
    blink_us: u32,
    /// The address field's measured run (render state, re-measured only when it changes).
    field: FieldRun,
}

struct SeerrState {
    /// The address the rows show: the stored one, or a write of this page's not yet answered.
    address: String,
    /// The Seerr account's email while the profile signs in with one (as stored, or as this page
    /// wrote it and the write is not yet answered); `None` for its Plex account.
    account: Option<String>,
    sel: RowKey,
    editing: bool,
}

impl LogicalState for SeerrState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0).str(&self.address).bool(self.editing);
        if let Some(email) = &self.account {
            w.str(email);
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("seerr sel={} set={} editing={} account={}", self.sel.0, !self.address.is_empty(), self.editing,
            self.account.is_some()));
    }
}

/// The caret's blink period, Search's.
const BLINK_US: u32 = 530_000;
/// The field's inset inside the Address row while it is typed into.
const FIELD_PAD: f32 = 24.0;

/// Where a field is typed: over its own row, which stands near the top of the
/// content column, well above the television's raised keyboard (`text_field::SYSTEM_KEYBOARD_TOP`),
/// which cannot be moved. `None` while the row is scrolled out of the table's frame.
fn field_rect(form: &FormTable<SeerrId, SeerrAction, SettingsPage>, id: &SeerrId) -> Option<Rect> {
    let i = form.index_of(id)? as i32;
    let row = form.table.row_rect(RouteLayout::screen().sectioned_table(), i)?;
    Some(Rect::new(row.x + FIELD_PAD, row.y, row.w - 2.0 * FIELD_PAD, row.h))
}

impl SeerrPage {
    pub(super) fn new(entry: EntryId, cx: &Cx<'_, InnerHost>) -> Self {
        let address = cx.views.seerr.map(|s| s.address().to_string()).unwrap_or_default();
        let account = cx.views.seerr.and_then(|s| account_email(s.account()));
        let status = status_text(&address, cx.views.seerr.map(|s| s.connection()));
        let mut page = Self {
            entry,
            form: FormTable::new(super::super::registry::BAND),
            state: SeerrState { address, account, sel: SeerrId::Address.key(), editing: false },
            draft: None,
            pending: None,
            method_pending: None,
            password: None,
            status,
            blink_us: 0,
            field: FieldRun::default(),
        };
        page.rebuild();
        page
    }

    fn rebuild(&mut self) {
        let keep = self.form.selected_id().cloned().unwrap_or(SeerrId::Address);
        self.form.table.compact = false;
        let form = seerr_form(&self.state.address, &self.status, self.state.account.as_deref(), self.password.is_some());
        self.form.set(form, Some(&keep));
        self.form.table.list_focused = true;
    }

    fn view(&self) -> TableScreen<'_> {
        TableScreen::new(
            Header::new(RouteLayout::screen(), Some(msg::settings_title()), msg::settings_seerr_title(), msg::settings_seerr_copy()),
            &self.form.table, GroupId(0), self.entry,
        ).keyed(&self.form)
    }

    fn keyboard(&mut self, up: bool, fx: &mut Effects<'_, InnerHost>) {
        self.state.editing = up;
        self.blink_us = 0;
        fx.push(Fx::Deliver(MachineId::Instance(InstanceId(0)), Delivery::Keyboard { up }));
        fx.invalidate(Provenance::Input);
    }

    fn start_typing(&mut self, id: SeerrId, fx: &mut Effects<'_, InnerHost>) {
        let text = match id {
            SeerrId::Email => self.state.account.clone().unwrap_or_default(),
            SeerrId::Password => self.password.clone().unwrap_or_default(),
            _ => self.state.address.clone(),
        };
        let end = text.len();
        self.draft = Some((id, TextBuffer::new(text, end)));
        self.keyboard(true, fx);
    }

    /// The keyboard goes down. `keep` keeps what was typed; otherwise the field is put back.
    fn stop_typing(&mut self, keep: bool, fx: &mut Effects<'_, InnerHost>) {
        if self.draft.is_none() {
            return;
        }
        self.keyboard(false, fx);
        self.finish_typing(keep, fx);
    }

    /// The draft ends (the keyboard is down or going): `keep` writes the address or the email, or
    /// holds the password for Sign in.
    fn finish_typing(&mut self, keep: bool, fx: &mut Effects<'_, InnerHost>) {
        let Some((id, draft)) = self.draft.take() else { return };
        if !keep {
            return;
        }
        let text = draft.text().trim().to_string();
        match id {
            SeerrId::Email => {
                if self.state.account.as_deref() != Some(text.as_str()) {
                    self.send_method(Some(text), fx);
                }
            }
            SeerrId::Password => {
                // A password is kept as typed: spaces are characters of it.
                self.password = (!draft.text().is_empty()).then(|| draft.text().to_string());
                self.rebuild();
            }
            _ => self.send(text, fx),
        }
    }

    /// Write the sign-in way (`Some(email)` a Seerr account) and show it meanwhile. Either change
    /// signs the profile out of the server: the next sign-in is the new way's.
    fn send_method(&mut self, account: Option<String>, fx: &mut Effects<'_, InnerHost>) {
        let (reply, receipt) = std::sync::mpsc::channel();
        fx.push(Fx::App(AppFx::Preferences(super::super::registry::PreferenceCmd::SeerrMethod {
            email: account.clone(), reply })));
        self.method_pending = Some((account.clone(), receipt));
        self.state.account = account;
        self.rebuild();
    }

    /// Write `address` through the preference door and show it meanwhile.
    fn send(&mut self, address: String, fx: &mut Effects<'_, InnerHost>) {
        if address == self.state.address {
            return;
        }
        let (reply, receipt) = std::sync::mpsc::channel();
        fx.push(Fx::App(AppFx::Preferences(super::super::registry::PreferenceCmd::SeerrAddress {
            address: address.clone(), reply })));
        self.pending = Some((address.clone(), receipt));
        self.state.address = address;
        self.rebuild();
    }

    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        let Some(action) = form_activate(&self.form, key, fx) else { return };
        let set = !self.state.address.is_empty();
        match action {
            SeerrAction::Address => self.start_typing(SeerrId::Address, fx),
            SeerrAction::Method if set => {
                let next = if self.state.account.is_some() { None } else { Some(String::new()) };
                self.password = None;
                self.send_method(next, fx);
            }
            SeerrAction::Email if set => self.start_typing(SeerrId::Email, fx),
            SeerrAction::Password if set => self.start_typing(SeerrId::Password, fx),
            SeerrAction::Connect if set => match (&self.state.account, &self.password) {
                (Some(email), Some(_)) if !email.is_empty() => {
                    let password = self.password.take().unwrap_or_default();
                    fx.push(Fx::App(AppFx::Preferences(super::super::registry::PreferenceCmd::SeerrSignIn {
                        email: email.clone(), password })));
                    self.rebuild();
                }
                (Some(_), _) => {}
                (None, _) => fx.push(Fx::App(AppFx::Preferences(super::super::registry::PreferenceCmd::SeerrConnect))),
            },
            SeerrAction::Method | SeerrAction::Email | SeerrAction::Password | SeerrAction::Connect => {}
            SeerrAction::Remove => self.send(String::new(), fx),
        }
    }

    /// Settle an answered write, then follow the stored address and the connection from the view.
    fn poll(&mut self, cx: &Cx<'_, InnerHost>) -> bool {
        if let Some((_, receipt)) = &self.pending {
            match receipt.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pending = None,
            }
        }
        if let Some((_, receipt)) = &self.method_pending {
            match receipt.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => self.method_pending = None,
            }
        }
        let mut changed = false;
        if let Some(seerr) = cx.views.seerr {
            // The sign-in way likewise: the store's (it reads the stored record every frame) once
            // this page's own write is answered.
            let account = account_email(seerr.account());
            if self.method_pending.is_none() && account != self.state.account {
                self.state.account = account;
                changed = true;
            }
            // With no write of its own in flight the rows read the stored address (the store has it
            // the frame after the write lands); a failed write so puts the old one back.
            if self.pending.is_none() && seerr.address() != self.state.address {
                self.state.address = seerr.address().to_string();
                changed = true;
            }
            let status = status_text(&self.state.address, Some(seerr.connection()));
            if status != self.status {
                self.status = status;
                changed = true;
            }
        }
        if changed {
            self.rebuild();
        }
        changed
    }

    /// A key while the keyboard is up: the panel's edit keys edit, OK keeps, BACK puts the address
    /// back; anything else is swallowed (focus stays on the field while typing).
    fn typing_key(&mut self, key: Key, sym: u32, fx: &mut Effects<'_, InnerHost>) {
        let edit = match (key, sym) {
            (Key::Left, _) => Some(TextEdit::Left),
            (Key::Right, _) => Some(TextEdit::Right),
            (_, plx_ui::consts::SDLK_BACKSPACE) => Some(TextEdit::Backspace),
            (_, plx_ui::consts::SDLK_CLEAR) => Some(TextEdit::Clear),
            _ => None,
        };
        if let Some(edit) = edit {
            self.edit(&edit, fx);
        } else if key == Key::Ok {
            self.stop_typing(true, fx);
        } else if key == Key::Back {
            self.stop_typing(false, fx);
        }
    }

    fn edit(&mut self, edit: &TextEdit, fx: &mut Effects<'_, InnerHost>) {
        if let Some((_, draft)) = &mut self.draft {
            draft.edit(edit);
            self.blink_us = 0;
            fx.invalidate(Provenance::Input);
        }
    }
}

impl Machine<InnerHost> for SeerrPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                fx.push(Fx::Deliver(
                    MachineId::Instance(InstanceId(0)),
                    Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::Elem(FocusKey { entry: self.entry, elem: self.state.sel.0 }),
                    })),
                ));
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                if self.poll(cx) {
                    fx.invalidate(Provenance::Input);
                }
                if self.state.editing {
                    let was_on = self.blink_us < BLINK_US;
                    self.blink_us = (self.blink_us + t.dt_us) % (2 * BLINK_US);
                    if was_on != (self.blink_us < BLINK_US) {
                        fx.invalidate(Provenance::Input);
                    }
                }
                self.form.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                if self.draft.as_ref().is_some_and(|(id, _)| to.elem != id.key().0) {
                    self.stop_typing(true, fx);
                }
                form_focus(&mut self.form, to.elem);
                if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) {
                    self.state.sel = key;
                }
                Handled::Yes
            }
            ScreenEvent::Activate(key) => {
                if self.draft.is_none() {
                    self.activate(*key, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::SystemKeyboard(up), .. }) => {
                // The panel closed by its own hand (its Done, or BACK on it): keep what was typed.
                if !*up && self.draft.is_some() {
                    self.state.editing = false;
                    self.finish_typing(true, fx);
                    fx.invalidate(Provenance::Input);
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Text(edit), .. }) if self.draft.is_some() => {
                self.edit(edit, fx);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, sym, edge, .. }, .. })
                if self.draft.is_some() && *edge != Edge::Up =>
            {
                self.typing_key(*key, *sym, fx);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Right, at_edge: true, .. }, .. }) => {
                if let Some(key) = cx.focus.current.and_then(|k| form_right_target(&self.form, k.elem)) {
                    self.activate(key, fx);
                }
                Handled::Yes
            }
            ScreenEvent::WillLeave(_) | ScreenEvent::Cover | ScreenEvent::Suspend | ScreenEvent::Unmount => {
                self.stop_typing(false, fx);
                if !matches!(ev, ScreenEvent::Cover) && self.password.take().is_some() {
                    self.rebuild();
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

plx_ui::focusable_via_view!(SeerrPage, InnerHost, view);

impl Screen<InnerHost> for SeerrPage {
    fn name(&self) -> &'static str { "seerr" }
    fn state(&self) -> &dyn LogicalState { &self.state }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { Some(Cow::Borrowed(msg::settings_title())) }
    fn prepare(&mut self, _b: &mut Budget, cx: &Cx<'_, InnerHost>) {
        let Some((id, draft)) = &self.draft else { return };
        let placeholder = match id {
            SeerrId::Email => msg::settings_seerr_email_title_c(),
            SeerrId::Password => msg::settings_seerr_password_title_c(),
            _ => msg::settings_seerr_address_title_c(),
        };
        // A password is drawn as one `*` per character, the caret after as many.
        let masked;
        let (text, caret) = if *id == SeerrId::Password {
            let before = draft.text()[..draft.caret()].chars().count();
            masked = "*".repeat(draft.text().chars().count());
            (masked.as_str(), before)
        } else {
            (draft.text(), draft.caret())
        };
        self.field.prepare(text, caret, placeholder, theme::size::TITLE, false, cx.measure);
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        plx_ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL);
        let Some((id, _)) = &self.draft else { return };
        let Some(field) = field_rect(&self.form, id) else { return };
        let p = f.painter;
        let row = Rect::new(field.x - FIELD_PAD, field.y, field.w + 2.0 * FIELD_PAD, field.h);
        p.rect(row, theme::CARD_RING_RAD, theme::SURFACE_PANEL, theme::SURFACE_PANEL, 0.0);
        let ink = if self.field.blank() { theme::TEXT_TERTIARY } else { theme::FIELD_EDITING_INK };
        text_field::draw(f, p, field, &self.field, ink, theme::TEXT_PRIMARY, true, self.blink_us < BLINK_US);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
}

/// The Seerr account's email while `account` is one, else `None` (its Plex account).
fn account_email(account: &SeerrAccount) -> Option<String> {
    match account {
        SeerrAccount::Account { email, .. } => Some(email.clone()),
        SeerrAccount::Plex => None,
    }
}

#[cfg(test)]
#[path = "settings_seerr_tests.rs"]
mod tests;
