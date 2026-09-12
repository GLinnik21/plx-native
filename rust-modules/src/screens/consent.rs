//! **Privacy & data, and the first-run consent question, as pages of the surface** (restructure
//! phase 5b; the words and the two-document preview design are `ui/consent.rs`'s, moved). One
//! type, two modes (§6.2 "mounts twice"): [`ConsentPage::settings`] is the Privacy & data page
//! under the Settings root — two toggles, five documents, Delete all local data, and a Done that
//! appears only once the draft differs from the stored answer; [`ConsentPage::first_run`] is one
//! STAGE of the sign-in's question — the reading list beside two equal answers — and the second
//! stage is a push of the same type carrying the first answer in its argument, so BACK from the
//! second is the surface's ordinary pop.
//!
//! The decision is NOT this screen's (§2.2, §2.3): an answer is `AppFx::Consent(Record)` to the
//! consent machine, which applies it and publishes the snapshot the telemetry threads read. A
//! half-made choice therefore cannot let an event through, exactly as the static `DRAFT` used
//! to guarantee.

use std::borrow::Cow;

use crate::telemetry::consent::{self, Consent};
use crate::ui::decision_alert::{Choice as AlertChoice, DecisionAlert};
use crate::ui::document_reader::DocumentReader;
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Delivery, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind,
    Key, LogicalState, Machine, MachineId, NavOp, PressFrom,
};
use crate::ui::route_screen::RouteLayout;
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusSource, FocusTarget,
    Focusable, GroupKind, GroupSpec, HitSource, Hover, Part, Placed, RenderStrategy, Screen,
    ScreenEvent, Seat, Step, Stop,
};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::table_screen::{BandPart, DocumentFocus, DocumentScreen, Header, TableScreen};
use crate::ui::widgets::CtlPop;
use crate::ui::{theme, Rect};

use super::family::{palette, table_focus, InnerHost, SettingsPage, ALERT_GROUP, BAND_GROUP, TABLE_GROUP};
use super::registry::{alert_index, band_index, word, AppFx, ConsentCmd, LoopReq, ALERT};

// ---- the words -------------------------------------------------------------------------------

const CRASH_TITLE: &str = "Share crash reports?";
const PRODUCT_TITLE: &str = "Share product analytics?";
const CRASH_BODY: &str = "If PlxNative crashes, or signing in fails, it can send technical details that help find and fix the problem. Reports may include the signal, code addresses, thread information and device compatibility details, or which sign-in step failed and how the connection answered, plus a random crash report identifier, created when you turn this on and deleted when you turn it off or sign out, so that repeated crashes under one crash report identifier are counted once rather than once each. They never include titles, Plex accounts, searches, server names or addresses, tokens, subtitle text, or the product analytics identifier.";
const PRODUCT_BODY: &str = "PlxNative can share which screens and features are used and broad sign-in and playback outcomes. Reports carry a random Analytics ID, created when you turn this on and deleted when you turn it off or sign out, and can include the app version, webOS version, television model and SoC, and whether a selected server is local, remote or relayed. They never include titles, Plex accounts, searches, server names or addresses, tokens, subtitle text, or exact viewing history.";
const ROW_ERRORS: &str = "Crash reports";
const ROW_ERRORS_SUB: &str = "Optional technical crash reports.";
const ROW_USAGE: &str = "Product analytics";
const ROW_USAGE_SUB: &str = "Optional feature and playback outcomes.";
const DOC_TITLE_CRASH: &str = "Crashes / Errors";
const DOC_TITLE_USAGE: &str = "Analytics / Usage";
const ROW_EXAMPLE: &str = "See an example report";
const ROW_POLICY: &str = "Privacy policy";
use super::legal::CONTACT_EMAIL;
const DOC_TITLE_ANALYTICS_ID: &str = "Analytics ID";
const DOC_TITLE_ERRORS_ID: &str = "Crash report ID";
const ROW_DELETE: &str = "Delete all local data";
const DELETE_SCOPE: &str = "This signs out and removes PlxNative data stored on this television. It does not delete data already sent to Plex, your Plex Media Servers, Sentry or PostHog.";
const CRUMB_SETTINGS: &str = "Settings";
const SETTINGS_TITLE: &str = "Privacy & data";
const SETTINGS_COPY: &str = "Control optional reporting, review exactly what may be shared, and manage data stored by PlxNative on this television.";

/// Should the app put the sign-in's question on screen?
///
/// Pure, and takes both inputs, so the harness rule is a host test rather than a hope. Getting
/// this wrong would not fail loudly: `tests/run.py` injects a token and expects Home, the fps
/// scenes grade a heartbeat on a known route, and every `sim-shot` script drives a screen it
/// chose — a consent prompt in front of any of them would quietly re-point the entire harness at
/// a screen nobody wrote an assertion for.
pub(crate) fn should_show(c: &Consent, automated: bool) -> bool {
    consent::should_ask(c, automated)
}

/// Which document a preview page shows (the low nibble of `SettingsPage::Preview`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PreviewKind {
    Crash,
    Usage,
    Policy,
    ErrorsId,
    AnalyticsId,
}

impl PreviewKind {
    const ALL: [Self; 5] = [Self::Crash, Self::Usage, Self::Policy, Self::ErrorsId, Self::AnalyticsId];
}

/// `SettingsPage::Preview`'s byte: the kind in the low nibble, `0x10` for the first-run crumb of
/// the crash stage, `0x20` for the product stage's.
const FIRST_RUN_CRASH: u8 = 0x10;
const FIRST_RUN_PRODUCT: u8 = 0x20;
/// `SettingsPage::ConsentStage`'s byte: the stage in bit 0, the first stage's answer in bit 4.
const STAGE_PRODUCT: u8 = 0x01;
const ERRORS_SHARED: u8 = 0x10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowId {
    Errors,
    Usage,
    PreviewCrash,
    PreviewUsage,
    Policy,
    ErrorsId,
    AnalyticsId,
    Delete,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Under the Settings root.
    Settings,
    /// The sign-in's question, stage 0 (crash) or 1 (product).
    FirstRun { product: bool, errors: bool },
}

pub(crate) struct ConsentPage {
    entry: EntryId,
    mode: Mode,
    table: TableView,
    rows: Vec<RowId>,
    /// The Settings draft (`errors`, `usage`) and what was stored when the page opened.
    draft: (bool, bool),
    base: (bool, bool),
    alert: DecisionAlert,
    /// The alert's two answers as drawn last (the map is the last DRAWN frame's, §7.6).
    alert_frames: std::cell::Cell<Option<(Rect, Rect)>>,
    pop: CtlPop<2>,
    state: ConsentState,
}

struct ConsentState {
    mode: u8,
    draft: (bool, bool),
    alert: bool,
}

impl LogicalState for ConsentState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.mode as u32).bool(self.draft.0).bool(self.draft.1).bool(self.alert);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("consent mode={} draft={:?} alert={}", self.mode, self.draft, self.alert));
    }
}

/// The translated pick of a fixed label: static C strings in, one pointer out, nothing
/// allocates on the draw path.
fn tr_c(en: &'static std::ffi::CStr, es: &'static std::ffi::CStr) -> &'static std::ffi::CStr {
    if crate::i18n::is_es() { es } else { en }
}

impl ConsentPage {
    /// Privacy & data, seeded from the published decision.
    pub(crate) fn settings(entry: EntryId, _cx: &Cx<'_, InnerHost>, _fx: &mut Effects<'_, InnerHost>) -> Self {
        let prev = consent::current().unwrap_or_default();
        let mut s = Self::bare(entry, Mode::Settings, (prev.errors, prev.usage));
        s.rebuild(0);
        s.table.list_focused = true;
        s
    }

    /// One stage of the first-run question.
    pub(crate) fn first_run(entry: EntryId, stage: u8, _cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Self {
        let mode = Mode::FirstRun {
            product: stage & STAGE_PRODUCT != 0,
            errors: stage & ERRORS_SHARED != 0,
        };
        let mut s = Self::bare(entry, mode, (false, false));
        s.rebuild(0);
        s.table.list_focused = false;
        // First run's whole point (the legacy module's doc, and this page's own
        // `first_run_answers_are_the_action_row_and_not_table_rows`) is that the two answers ARE
        // the interaction and the reading list beside them is only what you may read FIRST — so
        // focus has to open on the band, never on the list. This page cannot get that for free:
        // `family.rs` fixes `GroupId(0)` as the TABLE group for every page in the Settings
        // family, because every OTHER page here (the root, Legal, Favourite libraries, and this
        // same screen's own Settings mode) really does want to land on its list — and the
        // container's generic mount path always asks for exactly `ContainerGroup(GroupId(0))`
        // on a fresh entry (`ModalStack::present`'s own `Life::Ev`). So this is the one page in
        // the family that has to correct its own entry point, the same mechanism `row_commit`'s
        // Delete arm uses to trap focus on the alert: an `Enter` delivered to the surface's own
        // instance, which `RouteSurface::forward` re-addresses to itself and the outer engine
        // resolves for real against THIS page's `groups()` — see `request_band_focus`.
        Self::request_band_focus(fx);
        s
    }

    /// Ask the outer engine to re-seat on the answer band. `MachineId::Instance(InstanceId(0))`
    /// is a placeholder — `RouteSurface::forward` discards whatever instance id an inner page's
    /// `Enter` names and re-delivers it to the surface's own, exactly as it does for the alert's
    /// `Enter` in `row_commit` below.
    fn request_band_focus(fx: &mut Effects<'_, InnerHost>) {
        fx.push(Fx::Deliver(
            MachineId::Instance(crate::ui::machine::InstanceId(0)),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::ContainerGroup(BAND_GROUP),
            })),
        ));
    }

    fn bare(entry: EntryId, mode: Mode, base: (bool, bool)) -> Self {
        Self {
            entry,
            mode,
            table: TableView::new(),
            rows: Vec::new(),
            draft: base,
            base,
            alert: DecisionAlert::new(),
            alert_frames: std::cell::Cell::new(None),
            pop: CtlPop::new(),
            state: ConsentState {
                mode: match mode {
                    Mode::Settings => 0,
                    Mode::FirstRun { product: false, .. } => 1,
                    Mode::FirstRun { product: true, .. } => 2,
                },
                draft: base,
                alert: false,
            },
        }
    }

    fn row_ids(&self) -> Vec<RowId> {
        match self.mode {
            Mode::FirstRun { product, .. } => vec![
                if product { RowId::PreviewUsage } else { RowId::PreviewCrash },
                RowId::Policy,
            ],
            Mode::Settings => vec![
                RowId::Errors,
                RowId::Usage,
                RowId::PreviewCrash,
                RowId::PreviewUsage,
                RowId::Policy,
                RowId::ErrorsId,
                RowId::AnalyticsId,
                RowId::Delete,
            ],
        }
    }

    fn rebuild(&mut self, sel: i32) {
        self.rows = self.row_ids();
        self.table.header_ink = theme::TEXT_READING;
        let sections = match self.mode {
            Mode::FirstRun { .. } => vec![Section::new("")
                .row(Row::new(ROW_EXAMPLE).chevron(true))
                .row(Row::new(ROW_POLICY).chevron(true))],
            Mode::Settings => {
                let (errors, usage) = self.draft;
                vec![
                    Section::new("Reporting")
                        .row(Row::new(ROW_ERRORS).detail(ROW_ERRORS_SUB).toggle(errors))
                        .row(Row::new(ROW_USAGE).detail(ROW_USAGE_SUB).toggle(usage)),
                    Section::new("Information")
                        .row(Row::new(DOC_TITLE_CRASH).detail("Field-by-field preview of the crash/error report.").chevron(true))
                        .row(Row::new(DOC_TITLE_USAGE).detail("Field-by-field preview of product analytics events.").chevron(true))
                        .row(Row::new(ROW_POLICY).detail("The complete PlxNative privacy policy for this build.").chevron(true))
                        .row(Row::new(DOC_TITLE_ERRORS_ID).detail("The identifier on your crash reports, and how to have them deleted.").chevron(true))
                        .row(Row::new(DOC_TITLE_ANALYTICS_ID).detail("The identifier on your analytics, and how to have it deleted.").chevron(true)),
                    Section::new("On this TV").row(
                        Row::new(ROW_DELETE).detail("Sign out and remove PlxNative data from this TV.").chevron(true),
                    ),
                ]
            }
        };
        let keep = sel >= 0 && self.table.n_rows() > 0;
        self.table.set_sections(sections, sel.max(0), keep);
        self.state.draft = self.draft;
        debug_assert_eq!(self.rows.len() as i32, self.table.n_rows());
    }

    fn title(&self) -> &'static str {
        match self.mode {
            Mode::Settings => SETTINGS_TITLE,
            Mode::FirstRun { product: false, .. } => CRASH_TITLE,
            Mode::FirstRun { product: true, .. } => PRODUCT_TITLE,
        }
    }
    fn body(&self) -> &'static str {
        match self.mode {
            Mode::Settings => SETTINGS_COPY,
            Mode::FirstRun { product: false, .. } => CRASH_BODY,
            Mode::FirstRun { product: true, .. } => PRODUCT_BODY,
        }
    }
    fn crumb(&self) -> Option<&'static str> {
        match self.mode {
            Mode::Settings => Some(CRUMB_SETTINGS),
            Mode::FirstRun { product: false, .. } => None,
            Mode::FirstRun { product: true, .. } => Some(CRASH_TITLE),
        }
    }
    fn copy_size(&self) -> std::os::raw::c_int {
        match self.mode {
            Mode::Settings => theme::size::LABEL,
            Mode::FirstRun { .. } => theme::size::BODY,
        }
    }

    /// The band's labels this frame: Settings' Done only once the draft differs; first run's two
    /// equal answers. All four translate (i18n): static C-string pairs, one pointer pick each.
    fn band_labels(&self) -> Vec<&'static std::ffi::CStr> {
        match self.mode {
            Mode::Settings => {
                if self.draft != self.base {
                    vec![tr_c(c"Done", c"Hecho")]
                } else {
                    Vec::new()
                }
            }
            Mode::FirstRun { product, .. } => {
                if product {
                    vec![tr_c(c"Share analytics", c"Compartir analítica"), tr_c(c"Don’t share", c"No compartir")]
                } else {
                    vec![tr_c(c"Share reports", c"Compartir informes"), tr_c(c"Don’t share", c"No compartir")]
                }
            }
        }
    }

    fn list_frame(&self) -> Rect {
        let l = RouteLayout::screen();
        match self.mode {
            Mode::Settings => l.sectioned_table(),
            Mode::FirstRun { .. } => l.content,
        }
    }

    fn view(&self) -> ConsentView<'_> {
        let layout = RouteLayout::screen();
        let labels = self.band_labels();
        ConsentView {
            layout,
            table: &self.table,
            frame: self.list_frame(),
            entry: self.entry,
            labels,
            scales: [self.pop.scale(0), self.pop.scale(1)],
            alert_open: self.alert.is_open(),
            alert_frames: self.alert_frames.get(),
            alert_choice: self.alert.choice(),
            uncommitted: self.uncommitted(),
        }
    }

    /// Rule 9's guard (`ui/table_screen.rs`'s doc): **only Settings ever holds a draft BACK would
    /// discard** — this being hardcoded `false` unconditionally was exactly the bug that let LEFT
    /// overshoot Done into a change nobody asked to lose. First run answers nothing until OK is pressed —
    /// there is no draft sitting behind its band — so it is deliberately excluded here; the
    /// separate rule that keeps LEFT from escaping the UNANSWERED ceremony lives beside the
    /// `Key::Back` arm below, as an explicit `at_edge` check rather than this flag, because that
    /// case has to distinguish a synthetic edge-triggered BACK from a person's own BACK press —
    /// a distinction this boolean cannot express.
    fn uncommitted(&self) -> bool {
        matches!(self.mode, Mode::Settings) && self.draft != self.base
    }

    /// A band control was chosen: the first-run answer, or Settings' Done.
    fn band_commit(&mut self, i: usize, fx: &mut Effects<'_, InnerHost>) {
        match self.mode {
            Mode::Settings => {
                let (errors, usage) = self.draft;
                fx.push(Fx::App(AppFx::Consent(ConsentCmd::Record { errors, usage })));
                self.base = self.draft;
                fx.push(Fx::Nav(NavOp::Pop));
            }
            Mode::FirstRun { product: false, .. } => {
                let share = i == 0;
                fx.push(Fx::Nav(NavOp::Push(SettingsPage::ConsentStage(
                    STAGE_PRODUCT | if share { ERRORS_SHARED } else { 0 },
                ))));
                // The surface's own push (`RouteSurface::request`) mounts the Product stage and
                // THEN re-seats focus on its own default, `GroupId(0)` — which lands, and so
                // wins, AFTER the mount's own `request_band_focus` call (§7's effects drain
                // breadth-first, so whatever a step appends LAST is applied last). Asking again
                // here, ordered after the push above, lands after the surface's default too and
                // is what actually sticks: the second question must open on Share exactly as the
                // first one did, never on the reading list beside it.
                Self::request_band_focus(fx);
            }
            Mode::FirstRun { product: true, errors } => {
                fx.push(Fx::App(AppFx::Consent(ConsentCmd::Record { errors, usage: i == 0 })));
                // the ceremony is answered: the surface leaves
                fx.push(Fx::Nav(NavOp::Dismiss(EntryId(0))));
            }
        }
    }

    fn open_preview(&self, kind: PreviewKind, fx: &mut Effects<'_, InnerHost>) {
        let idx = PreviewKind::ALL.iter().position(|k| *k == kind).unwrap_or(0) as u8;
        let flag = match self.mode {
            Mode::Settings => 0,
            Mode::FirstRun { product: false, .. } => FIRST_RUN_CRASH,
            Mode::FirstRun { product: true, .. } => FIRST_RUN_PRODUCT,
        };
        fx.push(Fx::Nav(NavOp::Push(SettingsPage::Preview(idx | flag))));
    }

    fn row_commit(&mut self, row: i32, fx: &mut Effects<'_, InnerHost>) {
        let Some(id) = usize::try_from(row).ok().and_then(|i| self.rows.get(i)).copied() else {
            return;
        };
        match id {
            // **A flipped switch keeps focus on the row that was flipped.** This never needs to
            // ask the engine for anything: `row_commit` only runs on the row the OK was pressed
            // on, so focus was already on the table when the switch changed, never on Done — the
            // one case this file DOES have to correct by hand (`request_band_focus`, above) is a
            // fresh mount landing on the wrong group entirely, not an existing focus outliving
            // the control it was on.
            RowId::Errors => {
                self.draft.0 = !self.draft.0;
                self.rebuild(row);
            }
            RowId::Usage => {
                self.draft.1 = !self.draft.1;
                self.rebuild(row);
            }
            RowId::PreviewCrash => self.open_preview(PreviewKind::Crash, fx),
            RowId::PreviewUsage => self.open_preview(PreviewKind::Usage, fx),
            RowId::Policy => self.open_preview(PreviewKind::Policy, fx),
            RowId::ErrorsId => self.open_preview(PreviewKind::ErrorsId, fx),
            RowId::AnalyticsId => self.open_preview(PreviewKind::AnalyticsId, fx),
            RowId::Delete => {
                self.alert.open_with_body(tr_c(c"Delete all local data?", c"¿Borrar todos los datos locales?"), DELETE_SCOPE);
                self.state.alert = true;
                // the alert traps focus: seat the engine on its answers
                fx.push(Fx::Deliver(
                    MachineId::Instance(crate::ui::machine::InstanceId(0)),
                    Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::ContainerGroup(ALERT_GROUP),
                    })),
                ));
            }
        }
        fx.invalidate(crate::ui::present::Provenance::Input);
    }

    fn alert_answer(&mut self, destructive: bool, fx: &mut Effects<'_, InnerHost>) {
        if destructive {
            fx.push(Fx::App(AppFx::Loop(LoopReq::DeleteAllLocalData)));
            self.alert.dismiss();
        } else {
            self.alert.dismiss();
        }
        self.state.alert = false;
        fx.push(Fx::Deliver(
            MachineId::Instance(crate::ui::machine::InstanceId(0)),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::ContainerGroup(TABLE_GROUP),
            })),
        ));
        fx.invalidate(crate::ui::present::Provenance::Input);
    }
}

/// The page's focus composition: the table, the band (when it has controls) and — while the
/// delete alert is open — the alert's two answers ALONE (a modal traps focus, §7.3 step 7).
struct ConsentView<'a> {
    layout: RouteLayout,
    table: &'a TableView,
    frame: Rect,
    entry: EntryId,
    labels: Vec<&'static std::ffi::CStr>,
    scales: [f32; 2],
    alert_open: bool,
    alert_frames: Option<(Rect, Rect)>,
    /// The alert's own current answer (Cancel or Destructive), read alongside `alert_open` so
    /// `reconcile` can bounce a stray focus key back to wherever the alert ALREADY stood rather
    /// than to a hardcoded default — see that method's doc for the bug this closes.
    alert_choice: AlertChoice,
    uncommitted: bool,
}

impl<'a> ConsentView<'a> {
    fn screen(&'a self) -> TableScreen<'a> {
        let ts = TableScreen::new(
            Header::new(self.layout, None, "", ""),
            self.table,
            TABLE_GROUP,
            self.entry,
        )
        .uncommitted(self.uncommitted);
        ts.with_frame(self.frame).with_band(BandPart {
            layout: self.layout,
            labels: &self.labels,
            group: BAND_GROUP,
            entry: self.entry,
            uncommitted: self.uncommitted,
            scales: self.scales,
            palette: palette(),
            danger: None,
        })
    }
    fn alert_key(&self, i: usize) -> FocusKey<u32> {
        FocusKey {
            entry: self.entry,
            elem: ALERT + i as u32,
        }
    }
    fn alert_rect(&self, i: usize) -> Rect {
        match (self.alert_frames, i) {
            (Some((c, _)), 0) => c,
            (Some((_, d)), _) => d,
            (None, _) => Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }
}

impl Focusable<InnerHost> for ConsentView<'_> {
    fn groups(&self, cx: &Cx<'_, InnerHost>, out: &mut Vec<GroupSpec>) {
        if self.alert_open {
            out.push(GroupSpec {
                id: ALERT_GROUP,
                kind: GroupKind::Row { wrap: false },
                seat: Seat::First,
                reachable: AxisMask::BOTH,
                edge: [EdgeRule::Stop; 4],
                extent: self.alert_rect(0).union(self.alert_rect(1)),
                len: 2,
                elem: ElemKind::Control,
            });
            return;
        }
        Focusable::<InnerHost>::groups(&self.screen(), cx, out)
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, InnerHost>) -> Option<GroupId> {
        if self.alert_open {
            return alert_index(*key).map(|_| ALERT_GROUP);
        }
        Focusable::<InnerHost>::group_of(&self.screen(), key, cx)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, InnerHost>) -> Step<u32> {
        if self.alert_open {
            return match (alert_index(key.elem), dir) {
                (Some(0), Dir::Right) => Step::Move(self.alert_key(1)),
                (Some(1), Dir::Left) => Step::Move(self.alert_key(0)),
                _ => Step::Edge,
            };
        }
        Focusable::<InnerHost>::neighbour(&self.screen(), key, dir, cx)
    }
    fn place(&self, key: &u32, cx: &Cx<'_, InnerHost>, at: At) -> Option<Placed> {
        if self.alert_open {
            let i = alert_index(*key)?;
            let r = self.alert_rect(i);
            return Some(Placed {
                rect: r,
                rest_rect: r,
                clip: Rect::FULL,
                index: Some(i as u32),
            });
        }
        Focusable::<InnerHost>::place(&self.screen(), key, cx, at)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        if self.alert_open {
            // Whatever `want` names, focus must land back on ONE of the alert's two keys
            // while it is up — but WHICH one is the bug this branch used to get wrong.
            // `want` can legitimately be a stray non-alert key here: `FocusEngine::set`
            // (`ui/focus.rs`) parks ANY resolved pointer hit into the engine's persisted
            // scope unconditionally — its own doc describes a hover as "the engine records
            // it and answers the move for the owner to act on", with no mention of consulting
            // `Focusable::groups` first — and the table's own row stops keep registering on
            // every draw regardless of the scrim (see the `Activate` arm's doc above for why
            // a hit still resolves there). So a pointer sitting anywhere over the table, not
            // even clicked, reliably produces exactly this call with a non-alert `want`.
            // Before this fix the fallback for that case was the LITERAL constant 0 (Cancel),
            // which silently overwrote whatever the person had actually selected: a keyboard
            // user who arrow-key'd onto "Delete" and then only MOVED THE MOUSE over the page
            // underneath — no click, just a hover recomputing the pointer's nearest stop —
            // would find their choice reset to Cancel on the very next frame, for a reason
            // nothing on screen explained. The honest answer is the alert's OWN currently
            // selected choice (`alert_choice`, mirrored from `DecisionAlert::choice` in
            // `view()`): a stray `want` simply bounces off the alert exactly where it already
            // stood, rather than at whichever answer happens to be element 0.
            return self.alert_key(match alert_index(want.elem) {
                Some(i) => i,
                None => match self.alert_choice {
                    AlertChoice::Cancel => 0,
                    AlertChoice::Destructive => 1,
                },
            });
        }
        if alert_index(want.elem).is_some() {
            return FocusKey {
                entry: self.entry,
                elem: self.table.sel.max(0) as u32,
            };
        }
        Focusable::<InnerHost>::reconcile(&self.screen(), want, cx)
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        if self.alert_open {
            return self.alert_key(0);
        }
        Focusable::<InnerHost>::seat(&self.screen(), g, from, cx)
    }
}

crate::focusable_via_view!(ConsentPage, InnerHost, view);

impl Machine<InnerHost> for ConsentPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Tick(t) => {
                let dt = t.dt();
                self.table.update(dt, self.list_frame().h);
                self.alert.update(dt);
                let band = cx.focus.current.and_then(|k| band_index(k.elem));
                self.pop.step(band, dt);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                if let Some(i) = alert_index(to.elem) {
                    self.alert.set_choice(if i == 1 { AlertChoice::Destructive } else { AlertChoice::Cancel });
                } else {
                    table_focus(&mut self.table, to.elem);
                }
                Handled::Yes
            }
            ScreenEvent::Activate(e) => {
                // **The alert traps the pointer.** A row is `Activate::Direct` (`ui/table_screen.rs`'s
                // `TablePart::draw`), so a click anywhere on the table underneath the alert's scrim
                // still resolves a hit and reaches here — the engine's hit map has no notion of a
                // modal scrim, only of which stop drew on top. `ui/focus.rs`'s `set()` will even have
                // already written that row as `cx.focus.current` by the time this arrives. Refusing
                // the COMMIT here, unconditionally, is what actually stops a click from toggling a
                // switch, opening a preview or reaching Done while "Delete all local data?" is up —
                // reproducing legacy `pointer_focus`'s refusal (`!menu_open() || … || delete_alert()
                // .is_open() || …`) at the one point that matters, since this file cannot register or
                // un-register the library's own hit stops (`ui/table_screen.rs` is not ours to touch).
                //
                // **Gated on `visible()`, not `is_open()`.** `Popover::dismiss` — run the
                // instant ANY answer commits, Cancel included — flips `is_open()` false at
                // once, but the panel and its scrim keep drawing at falling alpha for the
                // whole exit fade, roughly half a second (`decision_alert.rs`'s own doc:
                // `visible()` is "open, or still fading out"; `Popover`'s module doc tells
                // callers in general to gate INPUT on `is_open()` and DRAWING on `visible()`,
                // which is right for a popover whose dismissal hands input back to something
                // that was already live underneath — a sheet over a page that never stopped
                // scrolling). That general rule is wrong for THIS caller specifically: the
                // alert's underneath is this same screen's own switches, Done and the Delete
                // row that opens this very alert again, so trusting `is_open()` here reopened
                // exactly the bug this whole arm exists to close — a click landing during the
                // fade could toggle a switch, open a document preview, or walk straight back
                // into "Delete all local data?" while its predecessor was still visibly
                // dissolving on screen.
                if self.alert.visible() {
                    return Handled::Yes;
                }
                if alert_index(*e).is_none() && band_index(*e).is_none() {
                    self.row_commit(*e as i32, fx);
                }
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                let Some(k) = cx.focus.current else {
                    return Handled::Yes;
                };
                if let Some(i) = alert_index(k.elem) {
                    // **Gate on the alert still being OPEN — `k.elem` naming one of its two
                    // keys is not, by itself, proof the alert is live.** `cx.focus.current` is
                    // the dispatcher's own steady-state record, and it can go stale for exactly
                    // one frame: the press machine's `Commit` events are appended to `head`
                    // AFTER a frame's own key-down events (`ui/dispatch.rs::frame_with`, steps
                    // 2 then 4), and a step's own emissions — including `alert_answer`'s re-seat
                    // onto `TABLE_GROUP` — join the BACK of the queue rather than running
                    // in-place (`Dispatcher::absorb`). So a BACK press that lands in the same
                    // (or an earlier) frame's FIFO ahead of an ALREADY-ARMED `PressCommit`
                    // against "Delete" runs first, dismisses the alert via the `Key::Back` arm
                    // below (`is_open()` flips false at once), and only THEN does the stale
                    // commit fire — still carrying `focus.current == ALERT + 1`, a closed
                    // alert's last-known selection, because the re-seat that would have moved
                    // it off that key is still waiting behind it in the queue. Answering
                    // unconditionally here read that stale selection and erased the
                    // television's local data after the person had already cancelled — a
                    // keyboard-only, narrow-window defect, but a destructive one, and the
                    // regression test below (`a_stale_presscommit_after_the_alert_was_already_
                    // dismissed_deletes_nothing`) reproduces the exact ordering rather than
                    // taking this reasoning on faith.
                    if self.alert.is_open() {
                        self.alert_answer(i == 1, fx);
                    }
                } else if !self.alert.visible() {
                    // Twin of the `Activate` guard above, for the band's own commit kind
                    // (`Activate::Press`, armed on key-down and committed on release) — gated
                    // on `visible()` rather than `is_open()` for the same reason that guard
                    // gives: the alert's dismissal FADE (`Popover::dismiss`, ~0.5 s) leaves
                    // `is_open()` false while the panel and its scrim are still on screen, and
                    // a stray band commit landing in that window — Done, or a first-run answer
                    // on the page underneath — must not fire while the alert is still visibly
                    // dissolving.
                    if let Some(i) = band_index(k.elem) {
                        self.band_commit(i, fx);
                    }
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key { key, edge: crate::ui::machine::Edge::Down, at_edge, .. },
                ..
            }) => {
                if self.alert.is_open() {
                    // the alert answers its own keys: BACK dismisses, OK is the press
                    if *key == Key::Back {
                        self.alert_answer(false, fx);
                        return Handled::Yes;
                    }
                    if matches!(key, Key::Up | Key::Down) {
                        return Handled::Yes;
                    }
                    return Handled::No;
                }
                if self.alert.visible() {
                    // **Trap the dismissal FADE too, not only the logically open alert.**
                    // `is_open()` above already went false the instant something answered it —
                    // this frame's own BACK arm just above, an earlier frame's OK against one
                    // of its two answers, or a stray click through the `Activate`/`PressCommit`
                    // guards elsewhere in this `step` — but `dismiss()`'s exit choreography
                    // leaves the panel and its scrim drawing at falling alpha for roughly half
                    // a second more (`decision_alert.rs`'s `visible()`: "open, or still fading
                    // out"). Falling through to the `match key` below during that window would
                    // let a RIGHT press at the table's trailing edge run `row_commit` on
                    // whatever row focus already sits on — opening a document preview, or,
                    // worse, RE-OPENING this very alert if focus is still on "Delete all local
                    // data" — while its predecessor is still visibly dissolving on screen.
                    // There is nothing left for the alert ITSELF to answer here (that already
                    // happened, above, on the frame `is_open()` went false), so this is a pure
                    // trap and nothing more: every key is swallowed until the fade lands.
                    return Handled::Yes;
                }
                match key {
                    // **A synthetic BACK is not a person's BACK.** `ui/dispatch.rs`'s edge-rule
                    // redelivery manufactures exactly this shape — `Key::Back`, `Edge::Down`,
                    // `at_edge: true` — when the band's LEFT edge resolves to `EdgeRule::Nav(Back)`
                    // and wants the input owner's first refusal; a real remote-control BACK press
                    // is built with `at_edge: false` always (`app/bridge.rs`), so the flag alone
                    // tells the two apart. They MUST diverge at a first-run stage: a real BACK is
                    // the platform's root-press signal at the crash stage and the wizard's own
                    // step-back at the product stage (both below, both tested), but a synthetic
                    // one only means "focus tried to leave the band leftward" — a directional
                    // gesture, not a request to leave the ceremony. Before this arm, that synthetic
                    // event fell into the SAME match below as a real press: at the crash stage a
                    // mere LEFT off "Share reports" would have asked the loop for the platform's
                    // root press exactly as a real BACK does, sending the television home on an
                    // arrow key; at the product stage it would have been declined here and walked
                    // the surface's own generic pop, silently dismissing the whole unanswered
                    // ceremony — the second half of the defect this file's audit found. Rule 9's
                    // `uncommitted` flag (see its doc) cannot express this: it changes the EDGE
                    // RULE the dispatcher installs, and first run has no draft for that flag's
                    // sense to attach to, so the wall belongs here instead, keyed on `at_edge`.
                    Key::Back if *at_edge && matches!(self.mode, Mode::FirstRun { .. }) => Handled::Yes,
                    Key::Back => match self.mode {
                        // rule: BACK at the FIRST question goes nowhere inside the app — the
                        // step behind it is sign-in — so it is the platform's (the root rule)
                        Mode::FirstRun { product: false, .. } => {
                            fx.push(Fx::App(AppFx::Loop(LoopReq::BackAtRoot)));
                            Handled::Yes
                        }
                        // Settings BACK discards the draft; the surface pops
                        _ => Handled::No,
                    },
                    Key::Right if *at_edge => {
                        if let Some(k) = cx.focus.current {
                            if self.table.row_opens(k.elem as i32) {
                                self.row_commit(k.elem as i32, fx);
                            }
                        }
                        Handled::Yes
                    }
                    _ => Handled::No,
                }
            }
            ScreenEvent::Enter(_) => Handled::Yes,
            _ => Handled::No,
        }
    }
}

impl Screen<InnerHost> for ConsentPage {
    fn name(&self) -> &'static str {
        match self.mode {
            Mode::Settings => word::PRIVACY,
            Mode::FirstRun { .. } => word::CONSENT,
        }
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> {
        ConsentPage::crumb(self).map(Cow::Borrowed)
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        let p = f.painter;
        let layout = RouteLayout::screen();
        Header::new(layout, self.crumb(), self.title(), self.body())
            .with_copy_size(self.copy_size())
            .paint(p, f.measure);
        let labels = self.band_labels();
        let alert_open = self.alert.is_open();
        // Rule 9's guard (see `Self::uncommitted`'s doc), threaded through the SAME two builder
        // sites `view()` feeds `ConsentView` — this `draw()` reconstructs an equivalent
        // `TableScreen`/`BandPart` by hand rather than going through `ConsentView::screen()`
        // (the two are drawn from the same fields but are not literally the same call), which is
        // exactly how this flag went hardcoded `false` here while `view()` carried the real one:
        // two call sites computing one fact independently is the shape that drifts.
        let uncommitted = self.uncommitted();
        {
            let mut ts = TableScreen::new(Header::new(layout, None, "", ""), &self.table, TABLE_GROUP, self.entry)
                .uncommitted(uncommitted)
                .with_frame(self.list_frame())
                .with_band(BandPart {
                    layout,
                    labels: &labels,
                    group: BAND_GROUP,
                    entry: self.entry,
                    uncommitted,
                    scales: [self.pop.scale(0), self.pop.scale(1)],
                    palette: palette(),
                    danger: None,
                });
            // the table and the band; the header was painted above with the page's own words
            Part::<InnerHost>::draw(&mut ts.table, f, self.list_frame());
            if let Some(b) = ts.band.as_mut() {
                Part::<InnerHost>::draw(b, f, layout.action);
            }
        }
        // the delete alert over everything, its own scrim first; its answers are the stops the
        // engine seats on while it is open
        if self.alert.visible() {
            self.alert.draw_scrim();
            self.alert.draw(tr_c(c"Cancel", c"Cancelar"), tr_c(c"Delete", c"Borrar"));
            let frames = self.alert.frames();
            self.alert_frames.set(Some(frames));
            // **Register the two hit stops only once the entrance spring has actually arrived.**
            // `frames()` is the FINAL layout — the panel `settled()` documents itself as reaching
            // — but `self.alert.draw` above paints it through the popover's own appear spring, so
            // for the first several frames after `open_with_body` the panel drawn on screen is
            // still displaced and nearly transparent. Registering a stop at the final rect on
            // frame one would let an impatient click land on "Delete" at a point where the panel
            // has not visually arrived yet — `ui::decision_alert`'s own doc names this as rule 11
            // and says every pointer caller gates on it; this is that gate, applied here.
            if alert_open && self.alert.settled() {
                for (i, r) in [frames.0, frames.1].into_iter().enumerate() {
                    f.stop(
                        crate::ui::Painter::root(),
                        Stop {
                            key: FocusKey {
                                entry: self.entry,
                                elem: ALERT + i as u32,
                            },
                            rect: r,
                            rest_rect: r,
                            clip: Rect::FULL,
                            hover: Hover::Focus,
                            activate: Activate::Press,
                        },
                    );
                }
            }
        }
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

// ---------------------------------------------------------------------------------------------
// the previews
// ---------------------------------------------------------------------------------------------

/// One Privacy & data document: a preview built through the real serialisers, the policy, or an
/// identifier page. Owns its text, so it is a `String` reader page rather than a `DocumentPage`.
pub(crate) struct PreviewPage {
    entry: EntryId,
    reader: DocumentReader,
    crumb: &'static str,
    title: &'static str,
    subtitle: &'static str,
    text: String,
    word: &'static str,
    state: PreviewState,
}

struct PreviewState {
    which: u8,
    /// The reading position — the same field, for the same reason, as `screens/legal.rs`'s
    /// `DocState::pos` over its own `DocumentReader` (that field's doc is the long version of
    /// this one). In short: `DocumentReader::at_top`/`at_end` (`ui/document_reader.rs:84-90`)
    /// read the reader's settled `target`, not its animating `scroll.pos`, to decide whether
    /// the NEXT `Key::Up`/`Key::Down` scrolls this page or is declined so the engine's edge rule
    /// can walk back out — so the reading position changes input ROUTING, not merely what is
    /// drawn, which makes it exactly the kind of fact a recorded-flow replay must be able to
    /// tell apart. Before this field, `PreviewState` hashed only `which`, so a recording of five
    /// `Key::Down`s through a long preview replayed `verdict=SAME` even against a build where
    /// `DocumentReader::move_by` had silently become a no-op — nothing else on this page's
    /// state changes as the document scrolls, so the divergence was invisible to the very
    /// mechanism built to catch it.
    ///
    /// `target` itself is a private field of `document_reader.rs`, which this file does not
    /// own, so this is a LOCAL MIRROR rather than a read of the real value — advanced in the
    /// exact same branch, under the exact same `at_top`/`at_end` guard, that calls `move_by`
    /// for real in `Machine::step` below, so the two can never disagree about whether a given
    /// key actually moved the document. Counted in whole `document_reader::STEP`s (one per
    /// `move_by` call, clamped at either end exactly where `at_top`/`at_end` themselves clamp)
    /// rather than pixels, for the same reason `DocState::pos` is: no float bits to reproduce,
    /// and no fractional-pixel difference that never changed which key handled next anyway.
    pos: u32,
}

impl LogicalState for PreviewState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.which as u32);
        w.u32(self.pos);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("preview {} pos={}", self.which, self.pos));
    }
}

impl PreviewPage {
    pub(crate) fn new(entry: EntryId, which: u8) -> Self {
        let kind = PreviewKind::ALL[(which & 0x0f) as usize % PreviewKind::ALL.len()];
        let (crumb, word) = if which & FIRST_RUN_CRASH != 0 {
            (CRASH_TITLE, word::CONSENT)
        } else if which & FIRST_RUN_PRODUCT != 0 {
            (PRODUCT_TITLE, word::CONSENT)
        } else {
            (SETTINGS_TITLE, word::PRIVACY)
        };
        let (title, subtitle): (&str, &str) = match kind {
            PreviewKind::ErrorsId => (
                DOC_TITLE_ERRORS_ID,
                "The random identifier attached to crash and error reports from this sign-in, and how to have those reports deleted.",
            ),
            PreviewKind::AnalyticsId => (
                DOC_TITLE_ANALYTICS_ID,
                "The random identifier attached to product analytics from this sign-in, and how to have those events deleted.",
            ),
            PreviewKind::Policy => (ROW_POLICY, "How PlxNative handles local data, Plex services and optional reporting."),
            PreviewKind::Crash => (
                DOC_TITLE_CRASH,
                "What is actually sent: the exact fields a crash or error report can carry — only when error reporting is on.",
            ),
            PreviewKind::Usage => (
                DOC_TITLE_USAGE,
                "What is actually sent: the exact fields a product analytics event can carry — only when usage reporting is on.",
            ),
        };
        let text = match kind {
            PreviewKind::ErrorsId => errors_id_document(),
            PreviewKind::AnalyticsId => analytics_id_document(),
            PreviewKind::Policy => super::legal::privacy_policy().to_string(),
            PreviewKind::Crash => preview_crash(),
            PreviewKind::Usage => preview_usage(),
        };
        Self {
            entry,
            reader: DocumentReader::new(),
            crumb,
            title,
            subtitle,
            text,
            word,
            state: PreviewState { which, pos: 0 },
        }
    }

    fn view(&self) -> DocumentFocus<'_> {
        DocumentFocus {
            reader: &self.reader,
            frame: RouteLayout::screen().document(true),
            group: TABLE_GROUP,
            entry: self.entry,
        }
    }
}

crate::focusable_via_view!(PreviewPage, InnerHost, view);

impl Machine<InnerHost> for PreviewPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, InnerHost>, _fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Tick(t) => {
                self.reader.update(t.dt());
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key { key: key @ (Key::Up | Key::Down), edge, .. },
                ..
            }) if *edge != crate::ui::machine::Edge::Up => {
                let inside = match key {
                    Key::Up => !self.reader.at_top(),
                    _ => !self.reader.at_end(),
                };
                if inside {
                    self.reader.move_by(if *key == Key::Up { -1 } else { 1 });
                    // Keep `PreviewState::pos` in lockstep with the move that just happened
                    // for real — see that field's doc for why this mirror, and not a read of
                    // the reader's own private `target`, is what gets hashed. `saturating_sub`
                    // is defensive rather than load-bearing: `inside` already proved
                    // `!at_top()` on the Up arm, i.e. `pos` cannot be 0 here, so the only way
                    // this saturates is a future edit that lets the mirror and the reader's
                    // real position drift apart — exactly the bug class this field exists to
                    // catch, so let it clamp instead of panicking.
                    if *key == Key::Up {
                        self.state.pos = self.state.pos.saturating_sub(1);
                    } else {
                        self.state.pos += 1;
                    }
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            _ => Handled::No,
        }
    }
}

impl Screen<InnerHost> for PreviewPage {
    fn name(&self) -> &'static str {
        self.word
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(self.crumb))
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        let Self { reader, crumb, title, subtitle, text, entry, .. } = self;
        let mut v = DocumentScreen::new(
            Header::new(RouteLayout::screen(), Some(crumb), title, subtitle),
            reader,
            text,
            TABLE_GROUP,
            *entry,
        );
        Part::<InnerHost>::draw(&mut v, f, Rect::FULL);
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

// ---- the payload previews (item 14: one document per telemetry channel) --------------------

/// **Item 14: the Crashes/Errors channel's own preview** — the native crash envelope, its two
/// fallback shapes and the handled-playback-error report, everything Sentry (Germany) can receive
/// when error reporting is on. Built through the real body serialisers and sanitizer, not a
/// mock-up: a field added to any of these schemas appears here, in front of the person being asked
/// to consent to it, the same argument the old combined `preview` made.
pub(crate) fn preview_crash() -> String {
    let mut out = String::from(
        "Crashes / Errors — what is actually sent to Sentry in Germany, and only when error \
         reporting is on. Random and build-specific values are placeholders; fixed classes below \
         are representative values from the closed domains in the Privacy notice. Nothing else is \
         sent. The crash report identifier is random, is created only when crash reports are \
         enabled, and is shown here as a placeholder.\n\n",
    );
    out.push_str("Native crash report (only when error reporting is on):\n");
    let crash = crate::telemetry::native::preview_event();
    let crash_text = serde_json::from_slice::<serde_json::Value>(&crash)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| String::from_utf8_lossy(&crash).into_owned());
    out.push_str(&crash_text);
    for (label, body) in crate::telemetry::crashreport::preview_events() {
        out.push_str("\n\n");
        out.push_str(label);
        out.push_str(" (only if native capture is unavailable):\n");
        let text = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| String::from_utf8_lossy(&body).into_owned());
        out.push_str(&text);
    }
    out.push_str("\n\nHandled playback error (only when error reporting is on):\n");
    let handled = crate::telemetry::playback::preview_event();
    let handled_text = serde_json::from_slice::<serde_json::Value>(&handled)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| String::from_utf8_lossy(&handled).into_owned());
    out.push_str(&handled_text);
    out.push_str(
        "\n\nSign-in problem report (automatically only when error reporting is on; otherwise \
         only when you press Send report, and then without the crash report identifier):\n",
    );
    let incident = crate::telemetry::incident::preview_event();
    let incident_text = serde_json::from_slice::<serde_json::Value>(&incident)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| String::from_utf8_lossy(&incident).into_owned());
    out.push_str(&incident_text);
    out.push_str("\n\n");
    out.push_str(&crate::telemetry::playback::preview_domains());
    out
}

/// **Item 14: the Analytics/Usage channel's own preview** — every
/// [`DiagEvent`](crate::diag::schema::DiagEvent) the build can emit, everything PostHog (Germany)
/// can receive when product analytics is on. Runs the real `posthog::preview` serialiser, so an
/// event added without being declared shows up here rather than only in a dashboard.
///
/// The identifier shown is always a placeholder, never the stored value. A new identifier is
/// minted only when product analytics is enabled; error-only consent creates none.
pub(crate) fn preview_usage() -> String {
    use crate::diag::schema::DiagEvent;
    let mut out = String::from(
        "Analytics / Usage — what is actually sent to PostHog in Germany, and only when usage \
         reporting is on, with a random Analytics ID. Random and build-specific values \
         are placeholders; fixed classes below are representative values from the closed domains \
         in the Privacy notice. Nothing else is sent. The usage identifier is random and is \
         created only when product analytics is enabled.\n\n",
    );
    out.push_str("Usage events (only when usage reporting is on):\n");
    for e in [
        DiagEvent::AppLaunch,
        DiagEvent::RouteEntered { screen: "home" },
        DiagEvent::SignInCompleted,
        DiagEvent::SignInStarted,
        DiagEvent::SignInFailed {
            kind: crate::diag::schema::SignInFailure::Authorization,
        },
        DiagEvent::SignInCancelled,
        DiagEvent::FeatureUsed {
            feature: crate::diag::schema::Feature::Seek,
        },
        // Representative values, not placeholders: every one of these is a real bucket the app can
        // actually emit, so what the person reads here is the shape of what would be sent. The
        // `playback_id` shown is the only number on the list, and its whole point is that it is a
        // fresh random one each time — see this screen's own no-32-hex-run assertion for the
        // property that matters, which is that no IDENTIFIER exists while this screen is up.
        DiagEvent::PlaybackRequested {
            playback_id: 4815162342,
        },
        DiagEvent::PlaybackStarted {
            playback_id: 4815162342,
            mode: "direct",
            raster: "fhd",
            fps: "24",
            video: "h264",
            audio: "ac3",
            startup: "1-3s",
        },
        DiagEvent::PlaybackFailed {
            playback_id: 4815162342,
            mode: "transcode",
            kind: "no_video_transcode_target",
        },
        DiagEvent::PlaybackCancelled {
            playback_id: 4815162342,
            mode: "direct",
        },
        DiagEvent::PlaybackAbandoned {
            playback_id: 4815162342,
            mode: "direct",
        },
        DiagEvent::PlaybackQuality {
            playback_id: 4815162342,
            rebuffers: "1",
            buffering: "<2s",
        },
        DiagEvent::PlaybackEnded {
            playback_id: 4815162342,
            mode: "direct",
            watched: "finished",
        },
    ] {
        let body =
            // The REAL environment this build would report, not a placeholder: it is the one
            // field on the preview that differs between a developer's build and a shipped one, and
            // showing the wrong side would make the panel lie about where the data goes.
            crate::telemetry::posthog::preview(
                "<project key>",
                "<random id>",
                e,
                crate::telemetry::sender::ENVIRONMENT,
            );
        // **Pretty-printed, and that is not cosmetic.** Compact JSON has almost no spaces, so a
        // greedy word-wrapper sees one enormous unbreakable word, fails to fit it, and ELIDES —
        // which the first capture of this panel showed as every object trailing off in "…", cutting
        // away the very fields it exists to display. Pretty-printing gives the wrapper real break
        // opportunities and gives a reader a structure they can scan.
        let text = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| String::from_utf8_lossy(&body).into_owned());
        out.push_str(&text);
        out.push_str("\n\n");
    }
    out
}

/// The union of both channels — test-only (`the_preview_shows_every_event_this_build_can_emit`).
#[cfg(test)]
fn preview() -> String {
    preview_crash() + &preview_usage()
}

/// The privacy policy this screen's Information section opens — **`ui::legal`'s document, not a
/// copy of it.** The row promises "the complete PlxNative privacy policy for this build", and the
/// only text that can keep that promise is the one the Legal notices index shows under the same
/// name.
///
/// It WAS a second literal here, and the two had drifted: this one had no `ON THIS TELEVISION`
/// section at all, so the door that described its document as complete opened the one omitting
/// what the app stores locally and what deleting it does. Nothing checks one `&'static str`
/// against another, which is why the guard is a test
/// (`both_privacy_policy_doors_open_the_same_document`) rather than a comment asking the next
/// editor to change two places.
/// The Analytics ID document — the identifier itself, what it is attached to, and the one process
/// that can act on a deletion request.
///
/// **It reads the STORED consent rather than the draft.** The draft is what the toggles currently
/// show, which may be an answer the person has not committed yet; the identifier that has actually
/// been sent with events is the one in `consent::current`. Showing a draft here would name an
/// identifier no event carries, or hide one that several do.
///
/// With analytics off there is no identifier to show, and that is the honest answer rather than a
/// blank: `consent::apply` sets `install_id: None` on withdrawal and mints a NEW one if analytics is
/// ever turned back on, so "off" really does mean the old handle is gone.
fn analytics_id_document() -> String {
    match consent::current().and_then(|c| c.install_id).as_deref() {
        Some(id) => format!(
            "YOUR ANALYTICS ID\n\n{id}\n\nWHAT IT IS\n\nA random identifier created on this television when you turned product analytics on. It is attached to analytics events so they can be counted as coming from one Analytics ID: one uninterrupted opt-in on one television. It is not derived from your Plex account, your television or anything about you, and it is never sent with crash reports, which carry a separate Crash report ID of their own.\n\nHOW TO HAVE THESE EVENTS DELETED\n\nWrite to {CONTACT_EMAIL} and quote the identifier above. It is the only handle these events carry, so a request without it cannot be matched to anything.\n\nHOW IT ENDS\n\nTurning product analytics off deletes this identifier, and turning analytics on again creates a different one. Signing out removes it as well, and the next person to sign in is asked afresh; so does Delete all local data. Events already sent keep the old identifier, which is why it is worth copying down before you turn analytics off if you intend to ask for their deletion."
        ),
        None => format!(
            "NO ANALYTICS ID\n\nProduct analytics is off, so this installation has no analytics identifier and is sending no analytics events.\n\nAn identifier is created only when you turn product analytics on, and deleting it is what turning it off does. If you had analytics on before and want events from that period deleted, write to {CONTACT_EMAIL} — but note that the identifier they carry was destroyed when analytics was turned off, so it can no longer be looked up from this television.\n\nCrash reports do not use this identifier. They carry a separate Crash report ID, shown on its own row while crash reports are on."
        ),
    }
}

/// The Crash report ID document — the crash channel's twin of [`analytics_id_document`], reading
/// the STORED decision for the same reason: the identifier that has actually gone out on reports
/// is the one in `consent::current`, not whatever the toggles currently show.
///
/// The one sentence that differs in kind from the analytics document is what the identifier is
/// FOR: it lets Sentry count how many Crash report IDs an issue reached — one per uninterrupted
/// opt-in on a television, since the id ends with the sign-in and with the switch — instead of
/// how many times it fired, which is the number that decides what gets fixed first. That is said plainly because it
/// is the reason the identifier exists, and a person deciding whether to leave the switch on is
/// owed the reason.
fn errors_id_document() -> String {
    match consent::current().and_then(|c| c.errors_id).as_deref() {
        Some(id) => format!(
            "YOUR CRASH REPORT ID\n\n{id}\n\nWHAT IT IS\n\nA random identifier created on this television when you turned crash reports on. It is attached to every crash and error report so that repeated crashes under one Crash report ID are counted once, which is what tells a problem that hit many people apart from one television that hit it many times. It is not derived from your Plex account, your television or anything about you, and it is never sent with product analytics, which has a separate Analytics ID of its own.\n\nHOW TO HAVE THESE REPORTS DELETED\n\nWrite to {CONTACT_EMAIL} and quote the identifier above. It is the only handle these reports carry, so a request without it cannot be matched to anything.\n\nHOW IT ENDS\n\nTurning crash reports off deletes this identifier, and turning them on again creates a different one. Signing out removes it as well, and the next person to sign in is asked afresh; so does Delete all local data. Reports already sent keep the old identifier, which is why it is worth copying down before you turn crash reports off if you intend to ask for their deletion."
        ),
        None => format!(
            "NO CRASH REPORT ID\n\nCrash reports are off, so this installation has no crash report identifier and is sending no crash or error reports.\n\nAn identifier is created only when you turn crash reports on, and deleting it is what turning them off does. If you had crash reports on before and want reports from that period deleted, write to {CONTACT_EMAIL} — but note that the identifier they carry was destroyed when crash reports were turned off, so it can no longer be looked up from this television."
        ),
    }
}

/// A test seam and a preview of the two `PressFrom`s the band answers to (the dispatcher arms
/// both the same way now; kept so the type stays named where the family's docs point).
#[allow(dead_code)]
fn press_from_named(_p: PressFrom) {}

#[cfg(test)]
#[path = "consent_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "consent_scope_tests.rs"]
mod scope_tests;

#[cfg(test)]
#[path = "consent_document_tests.rs"]
mod document_tests;

#[cfg(test)]
#[path = "consent_focus_tests.rs"]
mod focus_tests;

#[cfg(test)]
#[path = "consent_delete_alert_tests.rs"]
mod delete_alert_tests;
