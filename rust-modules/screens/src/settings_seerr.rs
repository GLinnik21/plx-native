//! **Settings > Services > Seerr** (docs/seerr.md): the household's Seerr server (Overseerr,
//! Jellyseerr), which the request surfaces ask on behalf of whoever is watching.
//!
//! Three rows: the server address (OK opens the television's keyboard on the address field drawn in
//! the page's panel, which stands above the raised keyboard), Connect (its sub-line is the live connection: connecting, connected as whom,
//! refused, unreachable) and Remove server. The page reads the stored address and the connection
//! from the Seerr store's view (`cx.views.seerr`), never from the session: the store is handed the
//! stored address every frame (`Bridge::seerr_pump`) and signs the seated profile in there.
//!
//! The write goes through `PreferenceCmd::SeerrAddress`: the field shows the typed address at once,
//! and the stored one settles it when the receipt answers. Connect is `PreferenceCmd::SeerrConnect`.
//! The keyboard is raised and lowered with `Delivery::Keyboard`, which the surface re-addresses to
//! itself (it is the input owner the dispatcher sees); the text and the panel's own close come back
//! as input. BACK while typing puts the address back; OK, the panel's close or leaving the row keeps
//! what was typed.

use super::*;
use plx_data::stores::seerr::Connection;
use plx_machine::machine::{Edge, InputEvent, InputKind, TextEdit};
use plx_platform::i18n::msg;
use plx_ui::text_buffer::TextBuffer;
use plx_ui::text_field::{self, FieldRun};

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum SeerrId {
    Address,
    Connect,
    Remove,
}

impl FormId for SeerrId {
    fn key(&self) -> RowKey {
        RowKey(match self {
            SeerrId::Address => 1,
            SeerrId::Connect => 2,
            SeerrId::Remove => 30,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum SeerrAction {
    Address,
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
        Some(Connection::Off | Connection::Idle) | None => msg::settings_seerr_status_idle().to_string(),
    }
}

/// The page's rows from plain inputs (the text-fit tests call this directly).
pub(super) fn seerr_form(address: &str, status: &str) -> Form<SeerrId, SeerrAction, SettingsPage> {
    let set = !address.trim().is_empty();
    let rows = FormSection::new("")
        .item(
            SeerrId::Address,
            RowKind::Button,
            SeerrAction::Address,
            Row::new(msg::settings_seerr_address_title())
                .detail(msg::settings_seerr_address_detail())
                .value(if set { address.trim() } else { msg::settings_seerr_address_unset() }),
        )
        .item(
            SeerrId::Connect,
            RowKind::Button,
            SeerrAction::Connect,
            Row::new(msg::settings_seerr_connect_title()).detail(status).dim(!set),
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
    /// While the keyboard is up: what is being typed, and where.
    draft: Option<TextBuffer>,
    /// An address written here and not yet answered: shown meanwhile, and the write's receipt.
    pending: Option<(String, std::sync::mpsc::Receiver<bool>)>,
    /// The Connect row's sub-line as last built, so a new connection rebuilds the rows once.
    status: String,
    blink_us: u32,
    /// The address field's measured run (render state, re-measured only when it changes).
    field: FieldRun,
}

struct SeerrState {
    /// The address the rows show: the stored one, or a write of this page's not yet answered.
    address: String,
    sel: RowKey,
    editing: bool,
}

impl LogicalState for SeerrState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0).str(&self.address).bool(self.editing);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("seerr sel={} set={} editing={}", self.sel.0, !self.address.is_empty(), self.editing));
    }
}

/// The caret's blink period, Search's.
const BLINK_US: u32 = 530_000;
/// The panel the address field stands in, in the narrative column under the copy, its foot a
/// [`theme::space::LG`] above the television's raised keyboard (`text_field::SYSTEM_KEYBOARD_TOP`):
/// the field is typed into while the keyboard is up, and the keyboard cannot be moved. It stands
/// there whether or not the keyboard is up, so nothing jumps when it rises.
const PANEL_H: f32 = 136.0;
const PANEL_PAD: f32 = 32.0;

fn panel() -> Rect {
    let narrative = RouteLayout::screen().narrative;
    let bottom = plx_ui::text_field::SYSTEM_KEYBOARD_TOP - theme::space::LG;
    Rect::new(narrative.x, bottom - PANEL_H, narrative.w, PANEL_H)
}

fn field_rect() -> Rect {
    let p = panel();
    Rect::new(p.x + PANEL_PAD, p.y + PANEL_PAD, p.w - 2.0 * PANEL_PAD, p.h - 2.0 * PANEL_PAD)
}

impl SeerrPage {
    pub(super) fn new(entry: EntryId, cx: &Cx<'_, InnerHost>) -> Self {
        let address = cx.views.seerr.map(|s| s.address().to_string()).unwrap_or_default();
        let status = status_text(&address, cx.views.seerr.map(|s| s.connection()));
        let mut page = Self {
            entry,
            form: FormTable::new(super::super::registry::BAND),
            state: SeerrState { address, sel: SeerrId::Address.key(), editing: false },
            draft: None,
            pending: None,
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
        self.form.set(seerr_form(&self.state.address, &self.status), Some(&keep));
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

    fn start_typing(&mut self, fx: &mut Effects<'_, InnerHost>) {
        let text = self.state.address.clone();
        let end = text.len();
        self.draft = Some(TextBuffer::new(text, end));
        self.keyboard(true, fx);
    }

    /// The keyboard goes down. `keep` writes what was typed; otherwise the address is put back.
    fn stop_typing(&mut self, keep: bool, fx: &mut Effects<'_, InnerHost>) {
        let Some(draft) = self.draft.take() else { return };
        self.keyboard(false, fx);
        if keep {
            self.send(draft.text().trim().to_string(), fx);
        }
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
        match action {
            SeerrAction::Address => self.start_typing(fx),
            SeerrAction::Connect if !self.state.address.is_empty() => {
                fx.push(Fx::App(AppFx::Preferences(super::super::registry::PreferenceCmd::SeerrConnect)));
            }
            SeerrAction::Connect => {}
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
        let mut changed = false;
        if let Some(seerr) = cx.views.seerr {
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
        if let Some(draft) = &mut self.draft {
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
                if to.elem != SeerrId::Address.key().0 {
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
                    if let Some(draft) = self.draft.take() {
                        self.send(draft.text().trim().to_string(), fx);
                    }
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
        let (text, caret) = match &self.draft {
            Some(draft) => (draft.text(), draft.caret()),
            None => (self.state.address.as_str(), self.state.address.len()),
        };
        self.field.prepare(text, caret, msg::settings_seerr_address_title_c(), theme::size::TITLE, false, cx.measure);
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        plx_ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL);
        let p = f.painter;
        let frame = panel();
        p.rect(frame, theme::CARD_RING_RAD, theme::scrim_black(0.30), theme::scrim_black(0.30), 0.0);
        let ink = if self.field.blank() {
            theme::TEXT_TERTIARY
        } else if self.state.editing {
            theme::FIELD_EDITING_INK
        } else {
            theme::TEXT_SECONDARY
        };
        text_field::draw(f, p, field_rect(), &self.field, ink, theme::TEXT_PRIMARY, self.state.editing, self.blink_us < BLINK_US);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
}

#[cfg(test)]
#[path = "settings_seerr_tests.rs"]
mod tests;
