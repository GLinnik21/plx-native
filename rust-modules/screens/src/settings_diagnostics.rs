//! **Settings > Diagnostics** (Settings v2): the playback-trace switch, the saved-log button and
//! Delete, one level below the root's single Diagnostics row (whose value reads Off or On).
//!
//! The rows are the ones the root used to carry, unchanged in what they do. What moved is their
//! words: the sentence under the title is the old sub-line of *Save diagnostic log*, and the one
//! sub-line left in the table is the switch's consequence ("May slow playback"), which has to
//! stay visible after the switch is turned on. Delete is last, in a section of its own, so the
//! table's hairline stands before it.
//!
//! Like every page of the family it states no IO: the three buttons leave as one
//! [`AppFx::PlaybackTrace`] and the status returns as an addressed `AppMsg::PlaybackTrace`.

use super::*;
use plx_platform::playback_trace::{Command, Status};

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum DiagId {
    Trace,
    Export,
    Delete,
}

impl FormId for DiagId {
    fn key(&self) -> RowKey {
        RowKey(match self {
            DiagId::Trace => 1,
            DiagId::Export => 2,
            DiagId::Delete => 3,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum DiagAction {
    Trace,
    Export,
    Delete,
}

/// The page's rows from the trace status alone.
pub(super) fn diagnostics_form(status: Status) -> Form<DiagId, DiagAction, SettingsPage> {
    use plx_platform::i18n::msg;
    let record = FormSection::new("")
        .disabled(status == Status::Saving)
        .item(DiagId::Trace, RowKind::Toggle, DiagAction::Trace,
            Row::new(msg::settings_trace_title()).detail(msg::settings_trace_detail()).toggle(status == Status::Recording))
        .item(DiagId::Export, RowKind::Button, DiagAction::Export,
            Row::new(msg::settings_trace_export()).value(trace_label(status)));
    let delete = FormSection::new("")
        .disabled(status == Status::Saving)
        .item(DiagId::Delete, RowKind::Button, DiagAction::Delete, Row::new(msg::settings_trace_delete()));
    Form::new().section(record).section(delete)
}

/// The root's Diagnostics value: the recording switch, as Off or On, both from the toggle's own
/// string family (a saved, saving or exported log is not a recording).
pub(super) fn root_value(status: Status) -> &'static str {
    if status == Status::Recording {
        plx_platform::i18n::msg::widgets_toggle_on()
    } else {
        plx_platform::i18n::msg::widgets_toggle_off()
    }
}

pub(super) struct DiagnosticsPage {
    entry: EntryId,
    /// `pub(super)` for the fit tests, which grade the table [`Self::at`] builds.
    pub(super) form: FormTable<DiagId, DiagAction, SettingsPage>,
    state: DiagState,
}

struct DiagState {
    sel: RowKey,
    status: Status,
}

impl LogicalState for DiagState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0).u8(self.status as u8);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("diagnostics sel={} status={:?}", self.sel.0, self.status));
    }
}

impl DiagnosticsPage {
    pub(super) fn new(entry: EntryId) -> Self {
        Self::at(entry, Status::Off)
    }

    /// The page as it stands once the runner has answered `status`: the table the fit tests grade
    /// in every status, the one a player sees.
    pub(super) fn at(entry: EntryId, status: Status) -> Self {
        let mut page = Self {
            entry,
            form: FormTable::new(super::super::registry::BAND),
            state: DiagState { sel: DiagId::Trace.key(), status },
        };
        page.rebuild();
        page
    }

    fn rebuild(&mut self) {
        let keep = self.form.selected_id().cloned().unwrap_or(DiagId::Trace);
        self.form.table.compact = false;
        self.form.table.board_metrics(true);
        self.form.set(diagnostics_form(self.state.status), Some(&keep));
        self.form.table.list_focused = true;
    }

    fn view(&self) -> TableScreen<'_> {
        use plx_platform::i18n::msg;
        TableScreen::new(
            Header::new(RouteLayout::settings_page(), Some(msg::settings_title()), msg::settings_trace_section(), msg::settings_trace_export_detail()),
            &self.form.table, GroupId(0), self.entry,
        )
        .keyed(&self.form)
    }

    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        let Some(action) = form_activate(&self.form, key, fx) else { return };
        let command = match action {
            DiagAction::Trace if self.state.status == Status::Recording => Command::Stop,
            DiagAction::Trace => Command::Start,
            DiagAction::Export => Command::Export,
            DiagAction::Delete => Command::Delete,
        };
        fx.push(Fx::App(AppFx::PlaybackTrace(command)));
    }
}

impl Machine<InnerHost> for DiagnosticsPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                // Seat the cursor explicitly, as the Language page does.
                fx.push(Fx::Deliver(
                    MachineId::Instance(InstanceId(0)),
                    Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::Elem(FocusKey { entry: self.entry, elem: self.state.sel.0 }),
                    })),
                ));
                // Read the status through the runner, never through a global in the screen.
                fx.push(Fx::App(AppFx::PlaybackTrace(Command::Status)));
                Handled::Yes
            }
            ScreenEvent::App(super::super::registry::AppMsg::PlaybackTrace(status)) => {
                if self.state.status != *status {
                    self.state.status = *status;
                    self.rebuild();
                    fx.invalidate(Provenance::Landing(MachineId::Session));
                }
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                if matches!(self.state.status, Status::Recording | Status::Saving) {
                    fx.push(Fx::App(AppFx::PlaybackTrace(Command::Status)));
                }
                self.form.table.update(t.dt(), RouteLayout::settings_page().sectioned_table().h);
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
            ScreenEvent::Input(plx_machine::machine::InputEvent {
                kind: plx_machine::machine::InputKind::Key { key: Key::Right, at_edge: true, .. }, ..
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

plx_ui::focusable_via_view!(DiagnosticsPage, InnerHost, view);

impl Screen<InnerHost> for DiagnosticsPage {
    fn name(&self) -> &'static str { "diagnostics" }
    fn state(&self) -> &dyn LogicalState { &self.state }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { Some(Cow::Borrowed(plx_platform::i18n::msg::settings_title())) }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        plx_ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
}
