//! **Settings > Integrations** (Radarr, Sonarr):
//!
//! 1. `IntegrationsPage`: lists Radarr and Sonarr with their configuration status.
//! 2. `ArrServicePage`: edits Server Address and API Token for a service, validates against
//!    the service's system status API, saves on success, and navigates back.

use std::borrow::Cow;
use std::sync::mpsc::Receiver;

use plx_data::integrations::{ArrKind, IntegrationsConfig};
use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, Fx, GroupId, Handled,
    InputEvent, InputKind, InstanceId, Key, LogicalState, Machine, MachineId, NavOp, TextEdit,
};
use plx_machine::present::Provenance;
use plx_ui::follow_copy::FollowCopy;
use plx_ui::form::{Form, FormId, FormSection, FormTable, RowKey, RowKind};
use plx_ui::frame::Budget;
use plx_ui::route_screen::RouteLayout;
use plx_ui::screen::{
    DrawFrame, FocusSource, HitSource, RenderStrategy, Screen, ScreenEvent,
};
use plx_ui::table::Row;
use plx_ui::table_screen::{Header, TableScreen};
use plx_ui::text_buffer::TextBuffer;
use plx_ui::{consts, Rect};

use super::{form_activate, form_focus, form_right_target, InnerHost, SettingsPage};
use crate::registry::{AppFx, PreferenceCmd};

// ============================================================================
// Integrations Index Page
// ============================================================================

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum IntegrationsRowId {
    Radarr,
    Sonarr,
}

impl FormId for IntegrationsRowId {
    fn key(&self) -> RowKey {
        RowKey(match self {
            Self::Radarr => 1,
            Self::Sonarr => 2,
        })
    }
}

pub(super) struct IntegrationsPage {
    entry: EntryId,
    pub(super) form: FormTable<IntegrationsRowId, (), SettingsPage>,
    state: IntegrationsState,
}

struct IntegrationsState {
    sel: RowKey,
    radarr_configured: bool,
    sonarr_configured: bool,
    radarr_url: String,
    sonarr_url: String,
}

impl LogicalState for IntegrationsState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0)
            .bool(self.radarr_configured)
            .bool(self.sonarr_configured);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "integrations sel={} radarr={} sonarr={}",
            self.sel.0, self.radarr_configured, self.sonarr_configured
        ));
    }
}

impl IntegrationsPage {
    pub(super) fn new(entry: EntryId) -> Self {
        let cfg = IntegrationsConfig::load();
        let mut page = Self {
            entry,
            form: FormTable::new(super::super::registry::BAND),
            state: IntegrationsState {
                sel: IntegrationsRowId::Radarr.key(),
                radarr_configured: cfg.radarr.is_configured(),
                sonarr_configured: cfg.sonarr.is_configured(),
                radarr_url: cfg.radarr.url,
                sonarr_url: cfg.sonarr.url,
            },
        };
        page.rebuild();
        page
    }

    fn reload_config(&mut self) {
        let cfg = IntegrationsConfig::load();
        let radarr_configured = cfg.radarr.is_configured();
        let sonarr_configured = cfg.sonarr.is_configured();
        if self.state.radarr_configured != radarr_configured
            || self.state.sonarr_configured != sonarr_configured
            || self.state.radarr_url != cfg.radarr.url
            || self.state.sonarr_url != cfg.sonarr.url
        {
            self.state.radarr_configured = radarr_configured;
            self.state.sonarr_configured = sonarr_configured;
            self.state.radarr_url = cfg.radarr.url;
            self.state.sonarr_url = cfg.sonarr.url;
            self.rebuild();
        }
    }

    fn service_value(configured: bool, url: &str) -> &'static str {
        if configured && !url.is_empty() {
            "Configured"
        } else {
            "Not configured"
        }
    }

    fn rebuild(&mut self) {
        let keep = self
            .form
            .selected_id()
            .cloned()
            .unwrap_or(IntegrationsRowId::Radarr);
        self.form.table.compact = false;
        self.form.table.board_metrics(true);

        let section = FormSection::new("")
            .item(
                IntegrationsRowId::Radarr,
                RowKind::Nav(SettingsPage::Radarr),
                (),
                Row::new("Radarr")
                    .value(Self::service_value(
                        self.state.radarr_configured,
                        &self.state.radarr_url,
                    ))
                    .chevron(true),
            )
            .item(
                IntegrationsRowId::Sonarr,
                RowKind::Nav(SettingsPage::Sonarr),
                (),
                Row::new("Sonarr")
                    .value(Self::service_value(
                        self.state.sonarr_configured,
                        &self.state.sonarr_url,
                    ))
                    .chevron(true),
            );

        self.form.set(Form::new().section(section), Some(&keep));
        self.form.table.list_focused = true;
    }

    fn view(&self) -> TableScreen<'_> {
        let copy = "Connect external services like Radarr and Sonarr to discover, monitor, and request movies and TV shows.";
        TableScreen::new(
            Header::new(
                RouteLayout::settings_page(),
                Some(plx_platform::i18n::msg::settings_title()),
                "Integrations",
                copy,
            ),
            &self.form.table,
            GroupId(0),
            self.entry,
        )
        .keyed(&self.form)
    }

    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        let _ = form_activate(&self.form, key, fx);
    }
}

impl Machine<InnerHost> for IntegrationsPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(
        &mut self,
        ev: &Self::Ev,
        cx: &Cx<'_, InnerHost>,
        fx: &mut Effects<'_, InnerHost>,
    ) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.reload_config();
                Handled::Yes
            }
            ScreenEvent::Enter(_) => {
                self.reload_config();
                Handled::No
            }
            ScreenEvent::Tick(t) => {
                self.form
                    .table
                    .update(t.dt(), RouteLayout::settings_page().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) {
                    self.state.sel = key;
                }
                Handled::Yes
            }
            ScreenEvent::Activate(key) => {
                self.activate(*key, fx);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent {
                kind:
                    InputKind::Key {
                        key: Key::Right,
                        at_edge: true,
                        ..
                    },
                ..
            }) => {
                if let Some(key) =
                    cx.focus.current.and_then(|k| form_right_target(&self.form, k.elem))
                {
                    self.activate(key, fx);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

plx_ui::focusable_via_view!(IntegrationsPage, InnerHost, view);

impl Screen<InnerHost> for IntegrationsPage {
    fn name(&self) -> &'static str {
        "integrations"
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

// ============================================================================
// Service Detail & Configuration Page (Radarr / Sonarr)
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EditingField {
    Address,
    Token,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum ServiceFormId {
    Address,
    Token,
    Submit,
}

impl FormId for ServiceFormId {
    fn key(&self) -> RowKey {
        RowKey(match self {
            Self::Address => 1,
            Self::Token => 2,
            Self::Submit => 3,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum ServiceAction {
    EditAddress,
    EditToken,
    Submit,
}

pub(super) struct ArrServicePage {
    entry: EntryId,
    kind: ArrKind,
    pub(super) form: FormTable<ServiceFormId, ServiceAction, SettingsPage>,
    state: ArrServiceState,
    url_draft: TextBuffer,
    token_draft: TextBuffer,
    editing: Option<EditingField>,
    receipt: Option<Receiver<Result<String, String>>>,
    follow: FollowCopy,
    sentence_buf: String,
}

struct ArrServiceState {
    sel: RowKey,
    busy: bool,
    error: Option<String>,
    success: Option<String>,
}

impl LogicalState for ArrServiceState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0)
            .bool(self.busy)
            .bool(self.error.is_some())
            .bool(self.success.is_some());
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "arr_service sel={} busy={} err={:?} ok={:?}",
            self.sel.0, self.busy, self.error, self.success
        ));
    }
}

impl ArrServicePage {
    pub(super) fn new(entry: EntryId, kind: ArrKind) -> Self {
        let cfg = IntegrationsConfig::load();
        let svc = cfg.service(kind);
        let url_text = svc.url.clone();
        let token_text = svc.token.clone();

        let mut page = Self {
            entry,
            kind,
            form: FormTable::new(super::super::registry::BAND),
            state: ArrServiceState {
                sel: ServiceFormId::Address.key(),
                busy: false,
                error: None,
                success: None,
            },
            url_draft: TextBuffer::new(url_text.clone(), url_text.len()),
            token_draft: TextBuffer::new(token_text.clone(), token_text.len()),
            editing: None,
            receipt: None,
            follow: FollowCopy::new(""),
            sentence_buf: String::new(),
        };
        page.rebuild();
        page
    }

    fn compute_sentence(&self) -> String {
        if self.state.busy {
            format!("Validating connection to {}...", self.kind.name())
        } else if let Some(err) = &self.state.error {
            format!("Connection error: {err}. Please check the server address and API key.")
        } else if let Some(ok) = &self.state.success {
            format!("Connected! {ok}")
        } else {
            match self.kind {
                ArrKind::Radarr => "Enter the server address (e.g. http://192.168.1.100:7878) and API key to connect Radarr for movie discovery and requests.".to_string(),
                ArrKind::Sonarr => "Enter the server address (e.g. http://192.168.1.100:8989) and API key to connect Sonarr for TV show discovery and requests.".to_string(),
            }
        }
    }

    fn masked_token(token: &str) -> String {
        if token.is_empty() {
            "Enter API key".to_string()
        } else if token.len() <= 6 {
            "••••••".to_string()
        } else {
            let visible_tail = &token[token.len() - 4..];
            format!("••••••••{visible_tail}")
        }
    }

    fn rebuild(&mut self) {
        let keep = self
            .form
            .selected_id()
            .cloned()
            .unwrap_or(ServiceFormId::Address);
        self.form.table.compact = false;
        self.form.table.board_metrics(true);

        // Address row detail
        let address_detail = match self.editing {
            Some(EditingField::Address) => self.url_draft.with_insertion_marker("|"),
            _ if self.url_draft.text().is_empty() => format!(
                "e.g. http://192.168.1.100:{}",
                self.kind.default_port()
            ),
            _ => self.url_draft.text().to_string(),
        };

        // Token row detail
        let token_detail = match self.editing {
            Some(EditingField::Token) => self.token_draft.with_insertion_marker("|"),
            _ => Self::masked_token(self.token_draft.text()),
        };

        // Submit row detail
        let submit_detail = if self.state.busy {
            "Connecting to server...".to_string()
        } else if self.state.error.is_some() {
            "Validation failed — fix details above and retry".to_string()
        } else if self.state.success.is_some() {
            "Connected successfully".to_string()
        } else {
            "Test connection and save settings".to_string()
        };

        let can_submit = !self.state.busy
            && !self.url_draft.text().trim().is_empty()
            && !self.token_draft.text().trim().is_empty();

        let address_row = Row::new("Server Address")
            .detail(address_detail)
            .dim(self.state.busy);
        let token_row = Row::new("API Key")
            .detail(token_detail)
            .dim(self.state.busy);
        let submit_row = Row::new("Validate & Save")
            .detail(submit_detail)
            .dim(!can_submit);

        let section = FormSection::new("")
            .item(
                ServiceFormId::Address,
                RowKind::Button,
                ServiceAction::EditAddress,
                address_row,
            )
            .item(
                ServiceFormId::Token,
                RowKind::Button,
                ServiceAction::EditToken,
                token_row,
            )
            .item(
                ServiceFormId::Submit,
                RowKind::Button,
                ServiceAction::Submit,
                submit_row,
            )
            .disabled(self.state.busy);

        self.form.set(Form::new().section(section), Some(&keep));
        self.form.table.list_focused = true;
        self.sentence_buf = self.compute_sentence();
        self.follow.follow(&self.sentence_buf);
    }

    fn persist_draft(&self, fx: &mut Effects<'_, InnerHost>) {
        fx.push(Fx::App(AppFx::Preferences(PreferenceCmd::SaveArrConfig {
            service: self.kind,
            url: self.url_draft.text().to_string(),
            token: self.token_draft.text().to_string(),
        })));
    }

    fn leave_field(&mut self, fx: &mut Effects<'_, InnerHost>) {
        if self.editing.is_none() {
            return;
        }
        self.editing = None;
        fx.push(Fx::Deliver(
            MachineId::Instance(InstanceId(0)),
            Delivery::Keyboard { up: false },
        ));
        self.persist_draft(fx);
        self.rebuild();
        fx.invalidate(Provenance::Input);
    }

    fn edit(&mut self, edit: &TextEdit, fx: &mut Effects<'_, InnerHost>) {
        match self.editing {
            Some(EditingField::Address) => self.url_draft.edit(edit),
            Some(EditingField::Token) => self.token_draft.edit(edit),
            None => return,
        }
        self.state.error = None;
        self.persist_draft(fx);
        self.rebuild();
        fx.invalidate(Provenance::Input);
    }

    fn submit(&mut self, fx: &mut Effects<'_, InnerHost>) {
        self.leave_field(fx);
        self.persist_draft(fx);
        let url = self.url_draft.text().trim().to_string();
        let token = self.token_draft.text().trim().to_string();
        if url.is_empty() || token.is_empty() {
            return;
        }

        self.state.busy = true;
        self.state.error = None;
        self.state.success = None;

        let (reply, receipt) = std::sync::mpsc::channel();
        self.receipt = Some(receipt);

        fx.push(Fx::App(AppFx::Preferences(PreferenceCmd::ValidateArr {
            service: self.kind,
            url,
            token,
            reply,
        })));
        self.rebuild();
        fx.invalidate(Provenance::Input);
    }

    fn poll_receipt(&mut self, fx: &mut Effects<'_, InnerHost>) {
        let Some(receipt) = &self.receipt else { return };
        match receipt.try_recv() {
            Ok(Ok(info)) => {
                self.state.busy = false;
                self.state.success = Some(info);
                self.state.error = None;
                self.receipt = None;
                self.rebuild();
                // Pop back to the Integrations list on valid submission!
                fx.push(Fx::Nav(NavOp::Pop));
                fx.invalidate(Provenance::Landing(MachineId::Session));
            }
            Ok(Err(err)) => {
                self.state.busy = false;
                self.state.error = Some(err);
                self.state.success = None;
                self.receipt = None;
                self.rebuild();
                fx.invalidate(Provenance::Landing(MachineId::Session));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.state.busy = false;
                self.state.error = Some("Internal error: validator disconnected".to_string());
                self.receipt = None;
                self.rebuild();
                fx.invalidate(Provenance::Landing(MachineId::Session));
            }
        }
    }

    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        let Some(action) = form_activate(&self.form, key, fx) else { return };
        match action {
            ServiceAction::EditAddress => {
                self.editing = Some(EditingField::Address);
                let text = self.url_draft.text().to_string();
                let end = text.len();
                self.url_draft = TextBuffer::new(text, end);
                fx.push(Fx::Deliver(
                    MachineId::Instance(InstanceId(0)),
                    Delivery::Keyboard { up: true },
                ));
                self.rebuild();
            }
            ServiceAction::EditToken => {
                self.editing = Some(EditingField::Token);
                let text = self.token_draft.text().to_string();
                let end = text.len();
                self.token_draft = TextBuffer::new(text, end);
                fx.push(Fx::Deliver(
                    MachineId::Instance(InstanceId(0)),
                    Delivery::Keyboard { up: true },
                ));
                self.rebuild();
            }
            ServiceAction::Submit => {
                self.submit(fx);
            }
        }
        fx.invalidate(Provenance::Input);
    }

    fn typing(
        &mut self,
        kind: &InputKind<u32>,
        _cx: &Cx<'_, InnerHost>,
        fx: &mut Effects<'_, InnerHost>,
    ) -> Handled {
        match kind {
            InputKind::SystemKeyboard(up) => {
                if !*up && self.editing.is_some() {
                    self.editing = None;
                    self.persist_draft(fx);
                    self.rebuild();
                    fx.invalidate(Provenance::Input);
                }
                Handled::Yes
            }
            InputKind::Text(edit) if self.editing.is_some() => {
                self.edit(edit, fx);
                Handled::Yes
            }
            InputKind::Key { key, sym, edge, .. } if self.editing.is_some() => {
                if *edge == Edge::Up {
                    return Handled::Yes;
                }
                let edit = match (*key, *sym) {
                    (Key::Left, _) => Some(TextEdit::Left),
                    (Key::Right, _) => Some(TextEdit::Right),
                    (_, consts::SDLK_BACKSPACE) => Some(TextEdit::Backspace),
                    (_, consts::SDLK_CLEAR) => Some(TextEdit::Clear),
                    _ => None,
                };
                match (edit, *key) {
                    (Some(edit), _) => self.edit(&edit, fx),
                    (None, Key::Back) => self.leave_field(fx),
                    (None, Key::Ok) => self.leave_field(fx),
                    (None, Key::Down) | (None, Key::Up) => {
                        self.leave_field(fx);
                        return Handled::No;
                    }
                    _ => {}
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    fn view(&self) -> TableScreen<'_> {
        TableScreen::new(
            Header::new(
                RouteLayout::settings_page(),
                Some("Integrations"),
                self.kind.name(),
                &self.sentence_buf,
            ),
            &self.form.table,
            GroupId(0),
            self.entry,
        )
        .keyed(&self.form)
    }
}

impl Machine<InnerHost> for ArrServicePage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(
        &mut self,
        ev: &Self::Ev,
        cx: &Cx<'_, InnerHost>,
        fx: &mut Effects<'_, InnerHost>,
    ) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                Handled::Yes
            }
            ScreenEvent::Enter(_) => {
                Handled::No
            }
            ScreenEvent::Tick(t) => {
                self.poll_receipt(fx);
                self.form
                    .table
                    .update(t.dt(), RouteLayout::settings_page().sectioned_table().h);
                self.follow.update(t.dt());
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind, .. }) => {
                if self.editing.is_some() || matches!(kind, InputKind::SystemKeyboard(_)) {
                    self.typing(kind, cx, fx)
                } else if let InputKind::Text(edit) = kind {
                    // Auto-enter edit mode if text input arrives while focused on an input row
                    if let Some(id) = self.form.selected_id() {
                        match id {
                            ServiceFormId::Address => {
                                self.editing = Some(EditingField::Address);
                                let text = self.url_draft.text().to_string();
                                let end = text.len();
                                self.url_draft = TextBuffer::new(text, end);
                                fx.push(Fx::Deliver(
                                    MachineId::Instance(InstanceId(0)),
                                    Delivery::Keyboard { up: true },
                                ));
                                self.edit(edit, fx);
                                Handled::Yes
                            }
                            ServiceFormId::Token => {
                                self.editing = Some(EditingField::Token);
                                let text = self.token_draft.text().to_string();
                                let end = text.len();
                                self.token_draft = TextBuffer::new(text, end);
                                fx.push(Fx::Deliver(
                                    MachineId::Instance(InstanceId(0)),
                                    Delivery::Keyboard { up: true },
                                ));
                                self.edit(edit, fx);
                                Handled::Yes
                            }
                            _ => Handled::No,
                        }
                    } else {
                        Handled::No
                    }
                } else if matches!(kind, InputKind::Key { key: Key::Right, at_edge: true, .. }) {
                    if let Some(key) = cx.focus.current.and_then(|k| form_right_target(&self.form, k.elem)) {
                        self.activate(key, fx);
                    }
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) {
                    self.state.sel = key;
                }
                self.follow.follow(&self.sentence_buf);
                Handled::Yes
            }
            ScreenEvent::Activate(key) => {
                self.activate(*key, fx);
                Handled::Yes
            }
            ScreenEvent::WillLeave(_) | ScreenEvent::Unmount | ScreenEvent::Cover => {
                self.leave_field(fx);
                self.persist_draft(fx);
                Handled::No
            }
            _ => Handled::No,
        }
    }
}

plx_ui::focusable_via_view!(ArrServicePage, InnerHost, view);

impl Screen<InnerHost> for ArrServicePage {
    fn name(&self) -> &'static str {
        match self.kind {
            ArrKind::Radarr => "radarr",
            ArrKind::Sonarr => "sonarr",
        }
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed("Integrations"))
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        plx_ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL);
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

    #[test]
    fn integrations_page_offers_radarr_and_sonarr_nav_rows() {
        let page = IntegrationsPage::new(EntryId(0));
        assert_eq!(page.form.table.n_rows(), 2);
        let radarr_binding = page.form.binding_at(0).expect("row 0 bound");
        assert_eq!(radarr_binding.kind, RowKind::Nav(SettingsPage::Radarr));
        let sonarr_binding = page.form.binding_at(1).expect("row 1 bound");
        assert_eq!(sonarr_binding.kind, RowKind::Nav(SettingsPage::Sonarr));
    }

    #[test]
    fn arr_service_page_fields_and_initial_state() {
        let page = ArrServicePage::new(EntryId(0), ArrKind::Radarr);
        assert_eq!(page.form.table.n_rows(), 3);
        assert_eq!(page.kind, ArrKind::Radarr);
        assert!(!page.state.busy);
        assert!(page.state.error.is_none());
    }

    #[test]
    fn masked_token_behavior() {
        assert_eq!(ArrServicePage::masked_token(""), "Enter API key");
        assert_eq!(ArrServicePage::masked_token("123"), "••••••");
        assert_eq!(ArrServicePage::masked_token("abcdefghij"), "••••••••ghij");
    }

    #[test]
    fn arr_service_editing_and_submit_flow() {
        let mut page = ArrServicePage::new(EntryId(0), ArrKind::Radarr);
        page.url_draft = TextBuffer::new("http://192.168.1.50:7878".to_string(), 24);
        page.token_draft = TextBuffer::new("testkey123".to_string(), 10);
        page.rebuild();
        assert!(!page.state.busy);
        assert!(page.state.error.is_none());

        // Check editing address
        page.editing = Some(EditingField::Address);
        page.url_draft.insert("/movies");
        assert_eq!(page.url_draft.text(), "http://192.168.1.50:7878/movies");
        page.editing = None;
        page.rebuild();

        // Check sentence reflects errors
        page.state.error = Some("Invalid API key (401)".to_string());
        page.rebuild();
        assert!(page.sentence_buf.contains("Invalid API key (401)"));
    }

    #[test]
    fn arr_service_persists_on_edit() {
        let mut page = ArrServicePage::new(EntryId(0), ArrKind::Sonarr);
        page.editing = Some(EditingField::Address);
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
        page.edit(
            &TextEdit::Commit(std::sync::Arc::from("http://sonarr.local:8989")),
            &mut fx,
        );

        let save_emitted = out.iter().any(|stamped| {
            matches!(
                &stamped.fx,
                Fx::App(AppFx::Preferences(PreferenceCmd::SaveArrConfig {
                    service: ArrKind::Sonarr,
                    url,
                    ..
                })) if url == "http://sonarr.local:8989"
            )
        });
        assert!(save_emitted, "typing must emit SaveArrConfig effect immediately");
    }
}

