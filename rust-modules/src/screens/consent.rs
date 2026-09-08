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
const CRASH_BODY: &str = "If PlxNative crashes, it can send technical details that help find and fix the problem. Reports may include the signal, code addresses, thread information and device compatibility details, plus a random crash report identifier, created when you turn this on and deleted when you turn it off or sign out, so that repeated crashes under one crash report identifier are counted once rather than once each. They never include titles, Plex accounts, searches, server names or addresses, tokens, subtitle text, or the product analytics identifier.";
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
    /// equal answers.
    fn band_labels(&self) -> Vec<&'static std::ffi::CStr> {
        match self.mode {
            Mode::Settings => {
                if self.draft != self.base {
                    vec![c"Done"]
                } else {
                    Vec::new()
                }
            }
            Mode::FirstRun { product, .. } => {
                if product {
                    vec![c"Share analytics", c"Don’t share"]
                } else {
                    vec![c"Share reports", c"Don’t share"]
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
                self.alert.open_with_body(DELETE_SCOPE);
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
            .paint(p);
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
            self.alert.draw(c"Delete all local data?", c"Cancel", c"Delete");
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
mod tests {
    use super::*;
    use crate::ui::fixture::FixtureMeasure;
    use crate::ui::machine::{
        Edge, FocusRead, InputOwner, InstanceId, PressId, PressRead, PresentHandle, Source, Stamped, Tick,
    };
    use crate::ui::present::Present;
    // `BAND` (the band's first element) is not otherwise imported into this file: the machine
    // itself only ever compares against it through `band_index`/`alert_index`, and this test
    // module is the one place that needs to construct a raw band `FocusKey` by hand, to drive a
    // `PressCommit`/synthetic-BACK event the way a real dispatch would.
    use crate::screens::registry::BAND;

    /// A bare `Cx<InnerHost>` for constructing or stepping a page with no SDL, no GL and no
    /// television — `InnerHost::Views<'a>` is `()`, so there is nothing to project, unlike
    /// `table_screen.rs`'s own `FixtureHost` version of this helper.
    fn test_cx(m: &FixtureMeasure) -> Cx<'_, InnerHost> {
        Cx {
            views: (),
            tick: Tick::default(),
            measure: m,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: InputOwner::Entry(EntryId(0)),
        }
    }

    /// A fresh effects sink's two halves, owned by the test so a page's `step`/constructor can be
    /// called more than once against the SAME buffer — the shape `RouteSurface::run_inner` builds
    /// for a real inner page (`Effects::from_handle`), minus the surface around it. The instance
    /// id is a placeholder: every `Fx::Deliver` this file emits targets one for the same reason
    /// `row_commit`'s Delete arm does — whatever surface forwards it re-addresses it to itself.
    fn sink() -> (Vec<Stamped<InnerHost>>, Present) {
        (Vec::new(), Present::new())
    }
    fn mk_fx<'a>(out: &'a mut Vec<Stamped<InnerHost>>, present: &'a mut Present) -> Effects<'a, InnerHost> {
        Effects::from_handle(out, MachineId::Instance(InstanceId(0)), PresentHandle::of(present))
    }

    fn key_down(key: Key) -> ScreenEvent<InnerHost> {
        ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: Source::Script,
            kind: InputKind::Key { key, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
        })
    }

    /// The shape `ui/dispatch.rs`'s edge-rule redelivery manufactures when the band's LEFT edge
    /// resolves to `EdgeRule::Nav(Back)`: the key is rewritten to `Key::Back` and `at_edge` is
    /// forced `true`, which is the ONLY way a real page ever sees that combination — a person's
    /// own BACK press is always built with `at_edge: false` (`app/bridge.rs`). Named for what it
    /// represents rather than what it literally is, so a test reads as "LEFT off the band's
    /// leading control", not as an unexplained `Key::Back`.
    fn synthetic_left_edge_back() -> ScreenEvent<InnerHost> {
        ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: Source::Script,
            kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: true },
        })
    }

    /// Undo a test's temporary `consent::install`, best-effort. **This is not a full restore.**
    /// `telemetry::consent` exposes `install` but no `uninstall`, so when the global held `None`
    /// before the test — nobody has loaded a decision this process yet — there is no way to put
    /// it back; the closest reachable state from here on is `Some(Consent::default())`, silently,
    /// for every test that runs afterward in this binary. `consent::current`'s own doc says why
    /// that distinction usually matters ("`None` means nothing has been loaded yet — distinct
    /// from 'a decision that allows nothing'"), and one production call site really does branch on
    /// it (`telemetry::mod.rs`'s `flush_soon`: `let Some(c) = consent::current() else { return }`).
    /// It is harmless for every GATE in this crate, which all fail closed on `None` exactly as
    /// they do on an explicit refusal — this is `[[test-suite-global-pollution]]`'s general shape,
    /// not a correctness bug in the screen. The clean fix is a `#[cfg(test)] fn uninstall()` on
    /// `telemetry::consent` (not this lane's file) so every call below could restore
    /// unconditionally instead of only when `saved` was already `Some`; centralised here so the
    /// gap is documented once rather than re-explained at each of the three call sites that used
    /// to inline this same conditional.
    fn restore_consent_snapshot(saved: Option<Consent>) {
        if let Some(saved) = saved {
            consent::install(saved);
        }
    }

    /// Whether any effect in `out` asks the engine to re-seat on `g` — the mechanism this page
    /// uses instead of moving focus itself (`row_commit`'s Delete arm, `alert_answer`, and the
    /// two `request_band_focus` call sites all emit exactly this shape).
    fn requests_group(out: &[Stamped<InnerHost>], g: GroupId) -> bool {
        out.iter().any(|s| {
            matches!(
                &s.fx,
                Fx::Deliver(
                    _,
                    Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::ContainerGroup(got),
                    })),
                ) if *got == g
            )
        })
    }

    // ---- should_show: the policy, byte for byte -------------------------------------------

    /// **An automated boot is never asked.** `tests/run.py` injects a token and expects Home, the
    /// fps scenes grade a heartbeat on a known route, and every `sim-shot` script drives a screen
    /// it chose — a consent prompt in front of any of them would silently re-point the whole
    /// harness at a screen nobody wrote an assertion for.
    #[test]
    fn an_automated_boot_never_sees_the_question() {
        assert!(!should_show(&Consent::default(), true));
        assert!(should_show(&Consent::default(), false), "…but an ordinary first boot does");
    }

    /// An answer against the current policy is not re-asked, whichever way it went.
    #[test]
    fn a_current_policy_answer_is_not_asked_again() {
        for (e, u) in [(false, false), (true, false), (false, true), (true, true)] {
            let answered = consent::apply(&Consent::default(), e, u, || Some("id".into()));
            assert!(!should_show(&answered, false), "re-asked after errors={e} usage={u}");
        }
    }

    /// A material schema expansion must receive two new explicit answers; the previous choice
    /// stays fail-closed until the first-run route asks the expanded question again.
    #[test]
    fn a_policy_bump_reasks_without_reusing_the_old_answer() {
        let old = Consent {
            asked_version: consent::POLICY_VERSION - 1,
            errors: true,
            usage: true,
            install_id: Some("old-id".into()),
            errors_id: Some("old-errors-id".into()),
        };
        assert!(should_show(&old, false));
        let current = consent::apply(&Consent::default(), true, false, || Some("new-id".into()));
        assert!(!should_show(&current, false));
    }

    /// Each channel carries its own identifier, so each is refused on its own failed mint —
    /// nothing here reads randomness itself; it only has to go through `consent::apply` with no
    /// second path, which is what this pins.
    #[test]
    fn unavailable_randomness_refuses_the_channel_it_failed_for() {
        let answer = consent::apply(&Consent::default(), true, true, || None);
        assert!(answer.answered(), "the person is not asked again");
        assert!(!answer.errors && !answer.usage);
        assert!(answer.install_id.is_none() && answer.errors_id.is_none());
    }

    // ---- the payload previews --------------------------------------------------------------

    /// **The preview is the real payload.** Every event this build can emit, and every one of its
    /// fields, must appear in front of the person being asked to consent to it.
    #[test]
    fn the_preview_shows_every_event_this_build_can_emit() {
        let text = preview();
        for s in crate::diag::schema::EVENT_SPECS {
            assert!(text.contains(s.name), "the payload preview does not show `{}`", s.name);
            for f in s.fields {
                assert!(
                    text.contains(f.key),
                    "the payload preview does not show `{}`'s field `{}`",
                    s.name,
                    f.key
                );
            }
        }
        for f in crate::diag::schema::CONTEXT_SPECS {
            assert!(text.contains(f.key), "the payload preview does not show context field `{}`", f.key);
        }
        for crash_field in ["stacktrace", "registers", "threads", "debug_meta", "image_size"] {
            assert!(text.contains(crash_field), "the native crash schema omits `{crash_field}`");
        }
        for fallback_field in ["C fault fallback", "Rust panic fallback", "fingerprint", "culprit"] {
            assert!(text.contains(fallback_field), "the fallback schema omits `{fallback_field}`");
        }
        for handled_field in [
            "Handled playback error",
            "handled",
            "breadcrumbs",
            "phase",
            "outcome",
            "requested_quality",
            "declared_rate",
            "media_rate",
            "picture presented",
            "seek requested",
            "quality selected",
            "delivery requested",
            "HLS request committed",
            "Original check phase",
            "playback failed",
        ] {
            assert!(text.contains(handled_field), "the handled playback-error schema omits `{handled_field}`");
        }
    }

    /// **The two identifier documents each name only their own channel's identifier.**
    #[test]
    fn each_identifier_document_shows_only_its_own_identifier() {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        let errors_id = "e".repeat(32);
        let analytics_id = "a".repeat(32);
        let mut draws = 0;
        consent::install(consent::apply(&Consent::default(), true, true, || {
            draws += 1;
            Some(if draws == 1 { errors_id.clone() } else { analytics_id.clone() })
        }));
        let errors_doc = errors_id_document();
        let analytics_doc = analytics_id_document();
        assert!(errors_doc.contains(&errors_id) && !errors_doc.contains(&analytics_id));
        assert!(analytics_doc.contains(&analytics_id) && !analytics_doc.contains(&errors_id));
        assert!(errors_doc.contains(CONTACT_EMAIL) && analytics_doc.contains(CONTACT_EMAIL));

        consent::install(consent::apply(&Consent::default(), false, false, || None));
        assert!(errors_id_document().starts_with("NO CRASH REPORT ID"));
        assert!(analytics_id_document().starts_with("NO ANALYTICS ID"));
        restore_consent_snapshot(saved);
    }

    /// **Item 14's whole point: the crash document carries nothing from the usage channel.**
    #[test]
    fn the_crash_preview_carries_nothing_from_the_usage_channel() {
        let text = preview_crash();
        assert!(!text.contains("distinct_id"), "no PostHog envelope field belongs in the crash-only document");
        assert!(!text.contains("<project key>"), "no PostHog project key belongs in the crash-only document");
        for s in crate::diag::schema::EVENT_SPECS {
            let quoted = format!("\"event\": \"{}\"", s.name);
            assert!(!text.contains(&quoted), "usage event `{}` leaked into the crash preview", s.name);
        }
    }

    /// The mirror image: no Sentry envelope shape belongs in the usage document.
    #[test]
    fn the_usage_preview_carries_nothing_from_the_crash_channel() {
        let text = preview_usage();
        for sentry_only in [
            "exception",
            "stacktrace",
            "registers",
            "threads",
            "debug_meta",
            "Handled playback error",
            "C fault fallback",
            "Rust panic fallback",
        ] {
            assert!(!text.contains(sentry_only), "crash-only field `{sentry_only}` leaked into the usage preview");
        }
    }

    /// The one property in the payload a reader could not otherwise verify.
    #[test]
    fn the_preview_shows_the_anonymity_flag() {
        assert!(preview().contains("$process_person_profile"));
        assert!(preview().contains("false"));
    }

    /// **The preview carries no real identifier**, on either route: first run cannot mint one
    /// before the usage answer, and Settings may already hold one but must never expose it here.
    #[test]
    fn the_preview_cannot_contain_a_real_identifier() {
        let text = preview();
        assert!(text.contains("created only when product analytics is enabled"), "the intro explains the placeholder");
        assert!(text.contains("<random id>"), "and the field itself is a placeholder");
        assert!(text.contains("created only when crash reports are enabled"), "the crash intro explains its placeholder");
        assert!(
            text.contains(crate::telemetry::native::PREVIEW_USER_ID),
            "and the crash-report id is shown as a placeholder"
        );
        let bytes: Vec<char> = text.chars().collect();
        let run = bytes
            .windows(32)
            .any(|w| w.iter().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert!(!run, "the preview contains something shaped like a real install id");
    }

    /// **The two Privacy-policy doors must open ONE document.** The legacy screen carried a
    /// SECOND literal that had already drifted from `ui::legal`'s own copy — the bug the original
    /// test was written against. This page cannot drift the same way: `PreviewKind::Policy`
    /// builds its text by calling `legal::privacy_policy()` directly rather than holding a copy,
    /// so the two are one function rather than two strings a reviewer has to keep in sync. The
    /// assertion is kept anyway, as a regression pin on the WIRING — "it can't drift, it's the
    /// same call" is exactly the kind of claim that quietly stops being true the next time
    /// someone "simplifies" the match arm.
    #[test]
    fn both_privacy_policy_doors_open_the_same_document() {
        let idx = PreviewKind::ALL.iter().position(|k| *k == PreviewKind::Policy).unwrap() as u8;
        let page = PreviewPage::new(EntryId(1), idx);
        assert_eq!(page.text, crate::screens::legal::privacy_policy());
    }

    /// **The regression `PreviewState::pos` exists to close.** Before that field, `PreviewState`
    /// hashed only `which`, so a recorded `Privacy & data -> "Crashes / Errors" -> DOWN x5` flow
    /// wrote five IDENTICAL state hashes: nothing else about this page changes as its document
    /// scrolls — the route and every other surface word are untouched, and the ONE element here
    /// (the reader) only scrolls, it never MOVES (`PreviewPage::step`'s own comment on the arm
    /// this test drives, and `screens/legal.rs`'s `a_down_inside_a_document_changes_the_hashed_
    /// state`, whose shape this test borrows for the sibling reader this file owns). A replay
    /// against that recording could not have told a working `move_by` apart from one that
    /// silently became a no-op, scrolled backwards, or moved by the wrong step — every one of
    /// those builds would have replayed `verdict=SAME` across the whole document.
    #[test]
    fn a_down_inside_a_preview_changes_the_hashed_state() {
        let _guard = crate::testlock::serial();
        let idx = PreviewKind::ALL.iter().position(|k| *k == PreviewKind::Crash).unwrap() as u8;
        let mut page = PreviewPage::new(EntryId(9), idx);
        // Long enough that five DOWNs (5 * `document_reader::STEP` = 960px) never reach the end,
        // so every one of them is a genuine mid-document scroll rather than a clamp at `at_end()`.
        page.reader.set_extent_for_test(5000.0);
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();

        let mut hashes = vec![page.state.hash()];
        for _ in 0..5 {
            let handled = page.step(&key_down(Key::Down), &c, &mut mk_fx(&mut out, &mut present));
            assert_eq!(handled, Handled::Yes);
            hashes.push(page.state.hash());
        }

        // Every consecutive pair must differ — a stuck, reversed or mis-stepped `move_by` leaves
        // at least one pair identical, which is precisely the false `verdict=SAME` the unfixed
        // field produced across a whole five-DOWN sequence.
        for w in hashes.windows(2) {
            assert_ne!(
                w[0], w[1],
                "a DOWN inside the preview must change the hashed state, not just the render position"
            );
        }

        // Walking back UP must retrace the exact same sequence of hashes in reverse: the hash is
        // a function of the reading POSITION (`PreviewState::pos`, mirroring the reader's
        // settled `target`), never of how many keys have been pressed or which direction they
        // came from — a counter that only ever incremented would pass the loop above while
        // still being wrong.
        for expect in hashes.iter().rev().skip(1) {
            let handled = page.step(&key_down(Key::Up), &c, &mut mk_fx(&mut out, &mut present));
            assert_eq!(handled, Handled::Yes);
            assert_eq!(
                page.state.hash(),
                *expect,
                "walking back UP must retrace the same positions, not accumulate a new one"
            );
        }
    }

    // ---- the row set: count AND order, per mode and per first-run stage --------------------

    /// The row list and the table it built cannot drift: a row added to one and not the other is
    /// the index bug that makes a menu act on the wrong line. Count alone would pass on two rows
    /// swapped, so order is asserted too, for every mode AND every first-run stage — the legacy
    /// test's own point was that the Product stage had never been exercised at all, so the one
    /// asymmetry `row_ids` can get wrong (which channel's preview a stage offers) went ungraded.
    #[test]
    fn every_row_id_has_a_row() {
        let _g = crate::testlock::serial();
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();

        let crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(crash.table.n_rows(), crash.rows.len() as i32, "first run, Crash stage");
        assert_eq!(crash.rows, vec![RowId::PreviewCrash, RowId::Policy], "Crash stage");

        out.clear();
        let product = ConsentPage::first_run(EntryId(1), STAGE_PRODUCT, &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(product.table.n_rows(), product.rows.len() as i32, "first run, Product stage");
        assert_eq!(product.rows, vec![RowId::PreviewUsage, RowId::Policy], "Product stage");

        out.clear();
        let settings = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(settings.table.n_rows(), settings.rows.len() as i32, "settings");
        assert_eq!(
            settings.rows,
            vec![
                RowId::Errors,
                RowId::Usage,
                RowId::PreviewCrash,
                RowId::PreviewUsage,
                RowId::Policy,
                RowId::ErrorsId,
                RowId::AnalyticsId,
                RowId::Delete,
            ],
            "settings"
        );
    }

    /// **Done is a route action after a change, never a table row** — a row appearing beside the
    /// toggles would move every row index below it, and reversing the edit must remove it again
    /// rather than leaving a stale action nobody can reach a live control for.
    #[test]
    fn toggling_a_switch_shows_done_without_resizing_the_table_and_reversing_hides_it_again() {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        consent::install(Consent::default());
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        assert!(page.band_labels().is_empty(), "no Done until a value differs from what is stored");
        let rows_before = page.table.n_rows();

        out.clear();
        page.row_commit(0, &mut mk_fx(&mut out, &mut present)); // row 0 is Crash reports
        assert_eq!(page.draft, (true, false));
        assert_eq!(page.table.n_rows(), rows_before, "Done never changes table geometry");
        assert_eq!(page.band_labels().len(), 1, "exactly one action appears");

        out.clear();
        page.row_commit(0, &mut mk_fx(&mut out, &mut present));
        assert_eq!(page.draft, (false, false), "toggled back to the stored answer");
        assert!(page.band_labels().is_empty(), "…and Done goes away with it");

        restore_consent_snapshot(saved);
    }

    /// **The answer commits through the consent MACHINE, never through this screen.** The screen
    /// emits `AppFx::Consent(Record)` and a `Pop`; it must never call
    /// `telemetry::consent::install`/`apply` itself, which is exactly what would let a half-made
    /// choice leak an event before the real machine has seen it.
    #[test]
    fn settings_done_commits_through_the_consent_machine_and_pops_the_surface() {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        consent::install(Consent::default());
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        page.draft = (true, true);

        out.clear();
        page.band_commit(0, &mut mk_fx(&mut out, &mut present));
        assert!(
            out.iter().any(|s| matches!(
                &s.fx,
                Fx::App(AppFx::Consent(ConsentCmd::Record { errors: true, usage: true }))
            )),
            "the answer must go out as a command to the consent machine"
        );
        assert!(out.iter().any(|s| matches!(&s.fx, Fx::Nav(NavOp::Pop))), "and the surface must pop");
        assert_eq!(
            consent::current(),
            Some(Consent::default()),
            "the screen itself must not have installed anything — only the machine reading \
             `AppFx::Consent` may do that"
        );

        restore_consent_snapshot(saved);
    }

    // ---- first run: the answers are the action row, not table rows ------------------------

    /// **The two answers are the ACTION ROW, and the list is only what you may read first.**
    /// Pinned because the failure is silent and cosmetic-looking: put an answer back among the
    /// rows and the screen still works, it just stops distinguishing deciding from reading.
    #[test]
    fn first_run_leaves_only_the_two_documents_in_its_reading_list() {
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(
            crash.rows,
            vec![RowId::PreviewCrash, RowId::Policy],
            "only the two readable documents remain in the list, and the preview is the crash \
             channel's own — Stage::Crash is where a fresh question always starts"
        );
        assert_eq!(crash.table.n_rows(), 2);
        assert_eq!(crash.band_labels().len(), 2, "first run always carries its two answers in the band");
    }

    /// **First run must open on its answers, not on the reading list — the bug this audit exists
    /// to catch.** `family.rs` fixes `GroupId(0)` as the TABLE group for every OTHER page in the
    /// Settings family (the root, Legal, Favourite libraries, and this same screen's own Settings
    /// mode all really do want to land on their list), and the container's generic mount always
    /// asks for exactly `ContainerGroup(GroupId(0))` on a fresh entry (`ModalStack::present`).
    /// Without an explicit correction the crash question would silently open with focus on "See
    /// an example report" instead of "Share reports" — a press of OK there would open a document
    /// instead of answering the question sitting in front of the person.
    #[test]
    fn first_run_requests_band_focus_on_its_own_mount() {
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let _crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
        assert!(
            requests_group(&out, BAND_GROUP),
            "the crash stage must ask to open on its answers, not the list beside them"
        );
    }

    /// **…and choosing an answer has to ask again, ordered after its own push.** The surface's
    /// generic push (`RouteSurface::request`) mounts the Product stage — which asks for the band
    /// on its own construction, exactly like the Crash stage above — and then re-seats focus on
    /// ITS OWN default, `GroupId(0)`; because that re-seat is emitted AFTER the mount finishes, it
    /// lands after (and so overrides) the mount's own request in the frame's effect queue. This
    /// only proves this page's OWN half of the fix — that `band_commit` orders its re-ask after
    /// its `Fx::Nav` — because the cross-file race with `RouteSurface::request`'s default lives in
    /// `screens/settings.rs`, which this lane does not own; see the audit's report for the traced
    /// argument that the ordering here is what makes the re-ask win that race.
    #[test]
    fn choosing_an_answer_re_asks_for_band_focus_after_its_own_push() {
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
        out.clear();
        crash.band_commit(0, &mut mk_fx(&mut out, &mut present)); // "Share reports"
        let nav_at = out
            .iter()
            .position(|s| matches!(s.fx, Fx::Nav(NavOp::Push(_))))
            .expect("a push to the product stage");
        let band_at = out
            .iter()
            .position(|s| requests_group(std::slice::from_ref(s), BAND_GROUP))
            .expect("a re-ask for the band");
        assert!(band_at > nav_at, "the re-ask must be ordered AFTER the push, or a competing default could win instead");
    }

    /// LEFT/RIGHT is the answer row's whole navigation and decides nothing on its own — only OK
    /// (`band_commit`) may write a decision. Declining must not carry the shared bit forward.
    #[test]
    fn declining_the_first_question_does_not_carry_the_shared_bit_forward() {
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
        out.clear();
        crash.band_commit(1, &mut mk_fx(&mut out, &mut present)); // "Don't share"
        assert!(
            out.iter().any(|s| matches!(
                &s.fx,
                Fx::Nav(NavOp::Push(SettingsPage::ConsentStage(bits))) if bits & ERRORS_SHARED == 0
            )),
            "declining crash reports must not answer the product question too"
        );
    }

    /// The second question's answer is combined with the FIRST one carried in its own page
    /// argument and committed as one `Record` — never a second draft, and never two commands.
    #[test]
    fn the_product_stage_combines_both_answers_into_one_record_and_dismisses_the_surface() {
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        // Crash answered "Share" (the shared bit set), carried into the Product stage's own arg.
        let mut product = ConsentPage::first_run(
            EntryId(1),
            STAGE_PRODUCT | ERRORS_SHARED,
            &c,
            &mut mk_fx(&mut out, &mut present),
        );
        out.clear();
        product.band_commit(0, &mut mk_fx(&mut out, &mut present)); // "Share analytics"
        assert!(out.iter().any(|s| matches!(
            &s.fx,
            Fx::App(AppFx::Consent(ConsentCmd::Record { errors: true, usage: true }))
        )));
        assert!(
            out.iter().any(|s| matches!(&s.fx, Fx::Nav(NavOp::Dismiss(_)))),
            "the ceremony is answered: the whole surface leaves"
        );
    }

    // ---- BACK: navigates; it never answers -------------------------------------------------

    /// **BACK at the crash stage is the root of the ceremony.** There is nothing behind it to
    /// restore — sign-in is the step behind it, and that cannot be undone — so it asks the LOOP
    /// for the root press instead of discarding anything or doing nothing silently. Unlike the
    /// legacy screen's `on_back` (a bare `bool` that could not tell "stepped back" and "swallowed"
    /// apart, which `app/run.rs` records as the one mechanical reason the 2026-09-03 root rule was
    /// never applied here), this page's `match self.mode` can, and does.
    #[test]
    fn back_at_the_crash_stage_asks_the_loop_for_the_root_press() {
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
        out.clear();
        let handled = crash.step(&key_down(Key::Back), &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(handled, Handled::Yes, "the crash stage answers BACK itself");
        assert!(
            out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Loop(LoopReq::BackAtRoot)))),
            "…by asking the loop for the root press, not by discarding anything"
        );
    }

    /// BACK at the Product stage is an ordinary step back, not a root press: this page declines
    /// it so the surface's own generic "pop if depth > 1" can run the reverse of the push.
    #[test]
    fn back_at_the_product_stage_declines_so_the_surface_pops_it() {
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut product = ConsentPage::first_run(EntryId(1), STAGE_PRODUCT, &c, &mut mk_fx(&mut out, &mut present));
        out.clear();
        let handled = product.step(&key_down(Key::Back), &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(handled, Handled::No, "declined so the surface's own stack pop can run");
    }

    /// Settings BACK is likewise declined here — discarding the draft is simply what popping an
    /// un-committed page does, and this page must not special-case it into its own dismissal.
    #[test]
    fn settings_back_is_declined_to_the_surfaces_own_pop() {
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        page.draft = (true, false);
        out.clear();
        let handled = page.step(&key_down(Key::Back), &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(handled, Handled::No);
        assert!(!out.iter().any(|s| matches!(&s.fx, Fx::Nav(_))), "this page pops nothing itself");
    }

    /// **The second half of this audit's LEFT-escape defect.** LEFT off the band's leading
    /// control resolves, through `ui/dispatch.rs`'s OWN edge-rule redelivery, to exactly this
    /// synthetic event — not a real BACK press (a real one always carries `at_edge: false`, see
    /// `synthetic_left_edge_back`'s doc). Before the `at_edge` arm in `step`'s `Key::Back` match,
    /// this fell into the SAME arm as a genuine press: at the crash stage it would have asked the
    /// loop for the platform's root press — sending the television home on an arrow key — and at
    /// the product stage it would have been declined, letting the surface's own generic pop
    /// dismiss the WHOLE unanswered ceremony. Both stages must instead treat it as a pure wall.
    #[test]
    fn left_off_the_bands_leading_control_is_a_wall_at_every_first_run_stage() {
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();

        let mut crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
        out.clear();
        let handled = crash.step(&synthetic_left_edge_back(), &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(handled, Handled::Yes, "the crash stage swallows it");
        assert!(out.is_empty(), "…and does nothing at all — not even the root-press a real BACK asks for");

        let mut product = ConsentPage::first_run(EntryId(1), STAGE_PRODUCT, &c, &mut mk_fx(&mut out, &mut present));
        out.clear();
        let handled = product.step(&synthetic_left_edge_back(), &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(handled, Handled::Yes, "the product stage swallows it too");
        assert!(out.is_empty(), "…and must not walk the wizard back to Crash or dismiss the surface");
    }

    // ---- the delete alert traps focus, and only the loop ever deletes ---------------------

    /// Opening the alert traps focus on it — the same mechanism `first_run`'s own mount fix uses,
    /// pointed at the alert's group instead of the band.
    #[test]
    fn the_delete_row_opens_the_alert_and_asks_to_be_reseated_on_it() {
        let _g = crate::testlock::serial();
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        let delete_row = page.rows.iter().position(|r| *r == RowId::Delete).unwrap() as i32;
        out.clear();
        page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));
        assert!(page.alert.is_open());
        assert!(requests_group(&out, ALERT_GROUP));
    }

    /// **Confirming asks the LOOP to sweep local data — this screen never touches disk itself.**
    /// The legacy screen's `take_delete_request` seam moved one level up, to `AppFx::Loop`.
    #[test]
    fn confirming_the_alert_asks_the_loop_to_delete_and_reseats_the_table() {
        let _g = crate::testlock::serial();
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        page.alert.open_with_body(DELETE_SCOPE);
        page.alert.set_choice(AlertChoice::Destructive);
        out.clear();
        page.alert_answer(true, &mut mk_fx(&mut out, &mut present));
        assert!(!page.alert.is_open(), "input modality ends on the press frame, same as every other popover");
        assert!(out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Loop(LoopReq::DeleteAllLocalData)))));
        assert!(requests_group(&out, TABLE_GROUP));
    }

    /// Cancelling never asks the loop for anything destructive.
    #[test]
    fn cancelling_the_alert_never_asks_the_loop_to_delete_anything() {
        let _g = crate::testlock::serial();
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        page.alert.open_with_body(DELETE_SCOPE);
        page.alert.set_choice(AlertChoice::Cancel);
        out.clear();
        page.alert_answer(false, &mut mk_fx(&mut out, &mut present));
        assert!(!page.alert.is_open());
        assert!(!out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Loop(LoopReq::DeleteAllLocalData)))));
    }

    /// **The blocker this audit exists to fix.** A click that lands on the table or the band
    /// underneath the "Delete all local data?" scrim resolves to the SAME `Activate`/`PressCommit`
    /// events a legitimate press sends — the engine's hit map has no notion of a modal scrim, and
    /// `ui/focus.rs`'s `set()` will already have written the clicked key as `cx.focus.current` by
    /// the time either arrives — so the guard has to live in the events themselves. This is the
    /// regression test for the two guards added to `Machine::step`'s `Activate` and `PressCommit`
    /// arms: before them, this test's two assertions on `draft`/`out` would have failed.
    #[test]
    fn the_open_alert_refuses_a_stray_activate_or_presscommit_on_the_table_or_band() {
        let _g = crate::testlock::serial();
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        let delete_row = page.rows.iter().position(|r| *r == RowId::Delete).unwrap() as i32;
        page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));
        assert!(page.alert.is_open());
        let errors_row = page.rows.iter().position(|r| *r == RowId::Errors).unwrap() as u32;
        let draft_before = page.draft;

        // A click landing on the "Crash reports" row underneath the scrim: the same event a real
        // pointer press on that row sends (`ui/dispatch.rs`'s `Activate::Direct` -> `Activate`).
        out.clear();
        let handled = page.step(&ScreenEvent::Activate(errors_row), &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(handled, Handled::Yes, "the event is consumed rather than left to fall through anywhere else");
        assert_eq!(draft_before, page.draft, "…but it must not toggle the switch underneath the scrim");
        assert!(page.alert.is_open(), "…nor must it close the alert");

        // A click landing on the band's leading control underneath the scrim: the same event a
        // real OK press or pointer release on it sends (`Activate::Press` -> `PressCommit`).
        out.clear();
        let mut cx_band = test_cx(&m);
        cx_band.focus.current = Some(FocusKey { entry: EntryId(1), elem: BAND });
        page.step(&ScreenEvent::PressCommit(PressId(0)), &cx_band, &mut mk_fx(&mut out, &mut present));
        assert!(
            !out.iter().any(|s| matches!(&s.fx, Fx::Nav(NavOp::Pop))),
            "a stray PressCommit on a band element must not pop the surface while the alert is open"
        );
        assert!(page.alert.is_open(), "…nor must it close the alert");
    }

    /// **Pins the alert's index mapping through the REAL dispatch path** —
    /// `ScreenEvent::PressCommit` routed by `cx.focus.current`, not the boolean shortcut
    /// `alert_answer(bool)` the two tests above call directly. Without this, `FocusMoved`'s
    /// mapping (`i == 1 => Destructive`), `ui/decision_alert.rs`'s two-element layout
    /// (`frames() -> (cancel, destructive)`) and `alert_answer`'s own `i == 1` test could all
    /// silently disagree about which element deletes, and every existing alert test would still
    /// pass unchanged — a click on Cancel would erase the television and `make check` would stay
    /// green.
    #[test]
    fn the_alert_index_mapping_is_pinned_through_a_real_press_commit() {
        let _g = crate::testlock::serial();
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        let delete_row = page.rows.iter().position(|r| *r == RowId::Delete).unwrap() as i32;

        // A freshly opened alert seats on Cancel (element 0) — `open_inner` resets `choice` to
        // `Choice::Cancel`, and `seat` is what the ENGINE actually calls on a fresh `Enter`, not a
        // property of `choice` this test could otherwise take on faith.
        out.clear();
        page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));
        assert!(page.alert.is_open());
        let placed = Placed {
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            rest_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            clip: Rect::FULL,
            index: None,
        };
        let seated = Focusable::<InnerHost>::seat(&page.view(), ALERT_GROUP, placed, &c);
        assert_eq!(seated.elem, ALERT, "a freshly opened alert seats on Cancel, element 0 — never the destructive answer");

        // Element 0 (Cancel) must never delete anything, driven through the real event `step`
        // dispatches — the shape `PressFrom::Pointer`/`PressFrom::Key` both funnel into.
        out.clear();
        let mut cx_cancel = test_cx(&m);
        cx_cancel.focus.current = Some(FocusKey { entry: EntryId(1), elem: ALERT });
        page.step(&ScreenEvent::PressCommit(PressId(0)), &cx_cancel, &mut mk_fx(&mut out, &mut present));
        assert!(
            !out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Loop(LoopReq::DeleteAllLocalData)))),
            "element 0 is Cancel and must never ask the loop to delete anything"
        );
        assert!(!page.alert.is_open(), "…but it does end the alert, same as every other answer");

        // Re-open fresh and pin the other end: element 1 (Destructive) is the ONLY one that may.
        out.clear();
        page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));
        let mut cx_delete = test_cx(&m);
        cx_delete.focus.current = Some(FocusKey { entry: EntryId(1), elem: ALERT + 1 });
        out.clear();
        page.step(&ScreenEvent::PressCommit(PressId(0)), &cx_delete, &mut mk_fx(&mut out, &mut present));
        assert!(
            out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Loop(LoopReq::DeleteAllLocalData)))),
            "element 1 is Destructive and must be the one that deletes"
        );
    }

    /// **The blocker this audit's verification pass found.** The dispatcher's own FIFO can
    /// deliver a BACK key-down (which dismisses the alert via `alert_answer(false, ..)`) BEFORE
    /// the `PressCommit` of an EARLIER OK-down against "Delete" that was already armed and
    /// sitting in the queue: `ui/dispatch.rs::frame_with` builds a frame's `head` with key-down
    /// events (step 2) ahead of the press machine's own `Commit`s (step 4), and a step's own
    /// emissions — including `alert_answer`'s re-seat onto `TABLE_GROUP` — join the BACK of the
    /// queue rather than running in place (`Dispatcher::absorb`). So the stale commit fires
    /// with `cx.focus.current` still frozen at the destructive key, one frame before the re-seat
    /// that would have moved it off ever runs. This test reproduces exactly that order by hand:
    /// dismiss the alert first (as the earlier-queued BACK would), THEN deliver the `PressCommit`
    /// with focus still pointing at the alert's destructive answer — the shape the real engine
    /// hands this page for that one frame. Before the `is_open()` guard inside the `alert_index`
    /// arm of `PressCommit`, this test's own assertion failed: the stale commit still asked the
    /// loop to erase the television's local data, after the person had already cancelled.
    #[test]
    fn a_stale_presscommit_after_the_alert_was_already_dismissed_deletes_nothing() {
        let _g = crate::testlock::serial();
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        let delete_row = page.rows.iter().position(|r| *r == RowId::Delete).unwrap() as i32;
        page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));
        assert!(page.alert.is_open());

        // Focus, for real, sits on the destructive answer — the same key an already-armed OK
        // press committed against.
        let mut cx_delete = test_cx(&m);
        cx_delete.focus.current = Some(FocusKey { entry: EntryId(1), elem: ALERT + 1 });

        // BACK arrives FIRST in this frame's FIFO and dismisses the alert — `is_open()` flips
        // false at once, though the re-seat that would move focus off the stale key is only
        // queued, not yet applied.
        out.clear();
        let handled = page.step(&key_down(Key::Back), &cx_delete, &mut mk_fx(&mut out, &mut present));
        assert_eq!(handled, Handled::Yes);
        assert!(!page.alert.is_open(), "the BACK press must dismiss the alert");

        // The STALE `PressCommit` — from the OK-down against "Delete" armed before the BACK
        // arrived — is delivered next, still carrying the destructive focus key. It must not
        // resurrect the answer the person just cancelled.
        out.clear();
        page.step(&ScreenEvent::PressCommit(PressId(0)), &cx_delete, &mut mk_fx(&mut out, &mut present));
        assert!(
            !out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Loop(LoopReq::DeleteAllLocalData)))),
            "a PressCommit that outlives the alert's own dismissal must not delete anything"
        );
    }

    /// **The dismissal fade is trapped exactly like the open alert, not only up to the frame
    /// `is_open()` flips false.** `Popover::dismiss` ends input modality on the SAME frame an
    /// answer commits but leaves the panel and its scrim drawing at falling alpha for roughly
    /// half a second more (`visible()`'s doc). This drives a real Cancel and then, with the
    /// alert still `visible()` (fading, never ticked forward), the three concrete things a
    /// stray press could have reached: a switch's `Activate`, the band's `PressCommit` (Done),
    /// and a `Key::Right` at a row's trailing edge — which, aimed at the Delete row itself,
    /// would silently RE-OPEN the very alert that is still dissolving on screen.
    #[test]
    fn the_dismissal_fade_traps_activate_presscommit_and_keys_the_same_as_the_open_alert() {
        let _g = crate::testlock::serial();
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        let delete_row = page.rows.iter().position(|r| *r == RowId::Delete).unwrap() as i32;
        page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));

        // Cancel: the alert dismisses (a fade begins) but stays `visible()` until that fade
        // lands — nothing here ever calls `alert.update`, so it never will during this test.
        out.clear();
        page.alert.set_choice(AlertChoice::Cancel);
        page.alert_answer(false, &mut mk_fx(&mut out, &mut present));
        assert!(!page.alert.is_open(), "Cancel closes the alert logically");
        assert!(page.alert.visible(), "…but it is still fading — the test is vacuous otherwise");

        // A click on a switch during the fade must not toggle it.
        let errors_row = page.rows.iter().position(|r| *r == RowId::Errors).unwrap() as u32;
        let draft_before = page.draft;
        out.clear();
        page.step(&ScreenEvent::Activate(errors_row), &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(draft_before, page.draft, "a click during the fade must not toggle the switch underneath it");

        // A stray band commit during the fade must not run Done, even with a real uncommitted
        // draft sitting behind it (fabricated here so there is a live Done to press at all).
        page.draft = (true, true);
        out.clear();
        let mut cx_band = test_cx(&m);
        cx_band.focus.current = Some(FocusKey { entry: EntryId(1), elem: BAND });
        page.step(&ScreenEvent::PressCommit(PressId(0)), &cx_band, &mut mk_fx(&mut out, &mut present));
        assert!(
            !out.iter().any(|s| matches!(&s.fx, Fx::Nav(NavOp::Pop))),
            "a stray band commit during the fade must not pop the surface (Done)"
        );

        // A RIGHT press at the Delete row's own trailing edge during the fade must not run
        // `row_commit` and re-open the very alert that is still fading out.
        out.clear();
        let mut cx_right = test_cx(&m);
        cx_right.focus.current = Some(FocusKey { entry: EntryId(1), elem: delete_row as u32 });
        page.step(
            &ScreenEvent::Input(InputEvent {
                at: Tick::default(),
                source: Source::Script,
                kind: InputKind::Key { key: Key::Right, sym: 0, wcode: 0, edge: Edge::Down, at_edge: true },
            }),
            &cx_right,
            &mut mk_fx(&mut out, &mut present),
        );
        assert!(!page.alert.is_open(), "a RIGHT press during the fade must not re-open the alert");
    }

    /// **The reconcile fallback must preserve the alert's OWN choice, never reset it to Cancel.**
    /// `FocusEngine::set` (`ui/focus.rs`) parks ANY resolved pointer hit into the engine's
    /// persisted scope unconditionally — it has no notion that a modal is up, and a pointer
    /// hovering the table underneath the alert's scrim still resolves against that table's own
    /// hit stops (the `Activate` arm's own doc explains why those keep registering). The
    /// dispatcher then asks this page's `reconcile` what to do with that stray key every frame,
    /// and `ConsentView::reconcile` correctly refuses to let focus actually land outside the
    /// alert while it is open — but before this fix it fell back to element 0 (Cancel)
    /// UNCONDITIONALLY whenever the stray key was not itself an alert key, silently overwriting
    /// whatever the person had chosen with the arrow keys. A keyboard user who had navigated
    /// onto "Delete" and then only moved the mouse — no click, just a hover recomputing the
    /// pointer's nearest stop — would find their selection reset to Cancel for no reason
    /// visible on screen.
    #[test]
    fn reconcile_keeps_the_alerts_own_choice_when_a_stray_key_names_something_outside_it() {
        let _g = crate::testlock::serial();
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        let delete_row = page.rows.iter().position(|r| *r == RowId::Delete).unwrap() as i32;
        page.row_commit(delete_row, &mut mk_fx(&mut out, &mut present));
        assert!(page.alert.is_open());

        // The person arrow-key'd onto the destructive answer.
        page.alert.set_choice(AlertChoice::Destructive);

        // A stray pointer hover on some unrelated table row — exactly what `FocusEngine::set`
        // parks into the engine's persisted scope with no regard for the modal being up.
        let stray = FocusKey { entry: EntryId(1), elem: 0 };
        let reconciled = Focusable::<InnerHost>::reconcile(&page.view(), stray, &c);
        assert_eq!(
            reconciled,
            FocusKey { entry: EntryId(1), elem: ALERT + 1 },
            "reconcile must keep focus on the alert's OWN current choice (Destructive), not reset it to Cancel"
        );
    }

    // ---- real dispatch events, not only the direct calls above ----------------------------

    /// A pointer click on a table row is `ScreenEvent::Activate`, dispatched by the engine
    /// (`ui/dispatch.rs`'s pointer-resolve, `Activate::Direct`) — every OTHER test in this file
    /// drives the row's effect through `row_commit` directly, which leaves the guard just added
    /// to `Machine::step`'s `Activate` arm (and the `alert_index`/`band_index` routing beside it)
    /// entirely ungraded by anything that looks like a real press.
    #[test]
    fn a_settings_toggle_commits_through_the_activate_event() {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        consent::install(Consent::default());
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        let errors_row = page.rows.iter().position(|r| *r == RowId::Errors).unwrap() as u32;

        out.clear();
        let handled = page.step(&ScreenEvent::Activate(errors_row), &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(handled, Handled::Yes);
        assert_eq!(page.draft, (true, false), "a real Activate — what a pointer click sends — must reach `row_commit`");

        restore_consent_snapshot(saved);
    }

    /// The band's twin of the test above: a real OK press or pointer release on a control is
    /// `ScreenEvent::PressCommit`, routed by `cx.focus.current` — every other first-run test
    /// drives `band_commit` directly. The alert's own mapping is pinned separately, above.
    #[test]
    fn a_first_run_answer_commits_through_the_presscommit_event() {
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));

        out.clear();
        let mut cx = test_cx(&m);
        cx.focus.current = Some(FocusKey { entry: EntryId(1), elem: BAND }); // "Share reports"
        let handled = crash.step(&ScreenEvent::PressCommit(PressId(0)), &cx, &mut mk_fx(&mut out, &mut present));
        assert_eq!(handled, Handled::Yes);
        assert!(
            out.iter().any(|s| matches!(
                &s.fx,
                Fx::Nav(NavOp::Push(SettingsPage::ConsentStage(bits))) if bits & ERRORS_SHARED != 0
            )),
            "a real PressCommit — what an OK press or pointer release sends — must reach `band_commit`"
        );
    }

    // ---- table motion: ported from legacy's own regression pins ---------------------------

    /// **Ported from legacy's `toggling_a_value_preserves_in_flight_focus_motion`.** A value-only
    /// `row_commit` calls `rebuild`, and `rebuild`'s `keep` expression (`let keep = sel >= 0 &&
    /// self.table.n_rows() > 0;`) exists so the rebuild does not reset the shared `TableView`'s
    /// highlight spring mid-flight — legacy's own test names the regression directly: "a
    /// value-only rebuild snapped the pill". Nothing in the restructured file exercised `keep`
    /// before this pin: every other row-commit test here only checks the resulting VALUE, never
    /// the spring, so the one-line re-derivation could regress silently.
    #[test]
    fn a_value_only_row_commit_preserves_in_flight_focus_motion() {
        let _g = crate::testlock::serial();
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        // Row 1 (Usage) is a toggle: move the shared table's selection there and let its
        // highlight spring start travelling before the value-only rebuild that flipping it
        // triggers.
        page.table.move_sel(1);
        page.table.update(1.0 / 60.0, page.list_frame().h);
        let moving = page.table.highlight_motion();

        out.clear();
        page.row_commit(1, &mut mk_fx(&mut out, &mut present)); // flips Usage, calls `rebuild(1)`
        assert_eq!(
            page.table.highlight_motion(),
            moving,
            "a value-only rebuild must not teleport the highlight spring — `rebuild`'s `keep` flag exists exactly for this"
        );
    }

    /// **Ported from legacy's `the_consent_update_advances_the_shared_table_focus_pill`** — filed
    /// there as "Regression for the TV report: the row selection changed its ink, but this screen
    /// never advanced the TableView springs". The risk carries over unchanged: `ScreenEvent::Tick`'s
    /// arm calls `self.table.update(dt, …)`, and nothing before this test drove a `Tick` through
    /// `step` to prove that call is actually REACHED, rather than only present in the source.
    #[test]
    fn a_tick_advances_the_shared_table_highlight_spring() {
        let _g = crate::testlock::serial();
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        page.table.move_sel(1);
        let before = page.table.highlight_motion();

        page.step(&ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }), &c, &mut mk_fx(&mut out, &mut present));
        let after = page.table.highlight_motion();
        assert!(
            after.0 != before.0 || after.1.abs() > 0.0,
            "a Tick delivered through the machine must advance the shared table's highlight spring: {before:?} -> {after:?}"
        );
    }

    // ---- content invariants: the two questions must not say the same thing ----------------

    /// The two switches must be identified by different words, in both their title and their
    /// sub-line — a copy-paste that made the crash and product rows share a label would still
    /// compile, still lay out, and would say the same thing to a person trying to decide what to
    /// share. Cheap, and worth having for exactly that reason: nothing else in this file would
    /// notice.
    #[test]
    fn the_two_switches_name_two_different_purposes() {
        assert_ne!(ROW_ERRORS, ROW_USAGE);
        assert_ne!(ROW_ERRORS_SUB, ROW_USAGE_SUB);
    }

    /// The prose carries the four things WP260's first layer needs — who, why, that it is
    /// optional, and where the rest is — plus the checkable payload claim, and the two questions
    /// disclose DIFFERENT identifiers rather than one being a paraphrase of the other. Asserted
    /// rather than eyeballed because a later edit for length is exactly how one of these goes
    /// missing without anyone noticing on screen — the constants are long paragraphs, not a
    /// caption a reviewer re-reads every time. (`privacy_policy()`'s own "names Sentry and
    /// PostHog" claim moved with the function itself, to `screens::legal`, in phase 5b — that
    /// half is that file's own invariant to keep now, not this one's.)
    #[test]
    fn first_run_separates_crash_and_product_consent() {
        assert!(CRASH_BODY.contains("signal"));
        assert!(CRASH_BODY.contains("product analytics identifier"));
        assert!(
            CRASH_BODY.contains("crash report identifier"),
            "the crash question must disclose the identifier it now carries"
        );
        assert!(PRODUCT_BODY.contains("random Analytics ID"));
        for body in [CRASH_BODY, PRODUCT_BODY] {
            assert!(
                body.contains("turn it off or sign out"),
                "each question must say the identifier ends with the sign-in, not with the television"
            );
        }
        assert!(PRODUCT_BODY.contains("exact viewing history"));
        assert_ne!(CRASH_TITLE, PRODUCT_TITLE);
    }

    // ---- a mounting owns its OWN table: no channel left for one page to leak into another --

    /// **Settings must open on its list whatever a first-run mounting left behind.** Legacy's
    /// `TABLE` was a crate `static mut`, so a Settings page built after a first-run page in the
    /// same process inherited whatever the static was left at — first run parks `list_focused =
    /// false` (its focus is the answer band), so the inherited value silently drew Settings with
    /// focus on nothing at all, while the keys still moved and committed an invisible selection.
    /// **That channel does not exist any more to leak through**: `ConsentPage::bare` (this file,
    /// above) builds a fresh `TableView::new()` into `self.table` on every call, so `first_run`
    /// and `settings` each own a table that belongs to their OWN struct instance rather than to
    /// the module. Proven here by holding both alive at once and mutating the first before the
    /// second is ever built — the shape a shared static would have to survive, and a plain
    /// owned-by-value struct field cannot: there is no code path left by which `settings`'s
    /// table could even SEE what `crash`'s table did, `move` and borrow-checking having already
    /// ruled it out at compile time, so the runtime assertions below are really about the
    /// CONSTRUCTORS' own defaults rather than about aliasing that the type system already makes
    /// impossible.
    #[test]
    fn settings_opens_on_the_list_after_a_first_run_left_focus_in_the_answer_band() {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        consent::install(Consent::default());
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();

        let mut crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
        assert!(!crash.table.list_focused, "first run parks focus on the answers, not the list");
        // Stand in for whatever a live session would have left a SHARED static at, were there
        // still one to leave anything at — a selection made on a totally different question.
        crash.table.move_sel(1);

        out.clear();
        let settings = ConsentPage::settings(EntryId(2), &c, &mut mk_fx(&mut out, &mut present));
        assert!(
            settings.table.list_focused,
            "…and a wholly independent Settings mounting must put focus back on its OWN list"
        );
        assert_eq!(settings.table.sel, 0, "…starting at its own top row, not `crash`'s leftover selection");

        restore_consent_snapshot(saved);
    }

    // ---- rule 11 on the answer band: a hover parks, it never answers -----------------------

    /// **Hovering never answers anything, on the one row this file still draws by hand.**
    /// Reported against Share Crash Reports, as part of the same navigation complaint the
    /// priority pins above answer: "hovering or arming answers nothing" was half of legacy's own
    /// hover test, pinned separately here because its OTHER half — which pixel resolves to which
    /// control, and that dead space resolves to none — is not this screen's mechanism any more.
    /// `BandPart`'s hover/click policy (`Hover::Focus`, `Activate::Press`,
    /// `ui/table_screen.rs`'s `BandPart::draw`) is the SAME generic stop every table row and
    /// every OTHER family screen's band already goes through (`ui/hit.rs`'s `HitMap`), so a
    /// regression in "does a hover park, does dead space park nothing" would break every band in
    /// the app at once, not this one alone. **It cannot be re-proven through a real dispatch on
    /// host at all**: the stops a pointer resolves against are only registered by a real `draw()`
    /// pass (`f.stop(...)` inside `BandPart::draw`), and drawing measures text through SDL2_ttf
    /// and issues GL calls that a host build has neither of — `screens::settings`'s own
    /// composed-test module says the same of the whole family (`draw: false` on every frame it
    /// runs, "not an optimization"). Settling THAT half needs `ui-sim` or a device. What stays
    /// this screen's own to prove, and provable with no drawing at all, is that the EVENT a
    /// hover produces — `ScreenEvent::FocusMoved` — never itself reaches `row_commit`/
    /// `band_commit`/`alert_answer`: only `Activate` and `PressCommit` may.
    ///
    /// **The name is deliberately narrower than the sentence at the top of this comment**, and
    /// the re-green pass that audited these pins is why. An earlier spelling of it —
    /// `hover_parks_an_answer_and_dead_space_parks_nothing` — promised the dead-space half that
    /// the paragraph above spends nine lines explaining this body cannot check: there is no
    /// `HitMap` here and no geometry, only a delivered `FocusMoved`. A test whose NAME claims
    /// more than its assertions do is the precise failure this phase's audit was sent to find
    /// (it found several), and it is worse than an owned gap, because the next reader greps for
    /// "dead space", finds a green test, and stops looking. Legal's sibling
    /// `a_hover_over_a_real_row_parks_it_and_a_click_opens_the_row_the_pointer_landed_on` is
    /// where a real `HitMap` over real row geometry IS driven, dead space included; the half
    /// that needs a real `draw()` is `ui-sim`'s or the television's, as above.
    #[test]
    fn a_hover_over_the_answer_band_parks_without_answering_anything() {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        consent::install(Consent::default());
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
        let draft_before = page.draft;

        out.clear();
        let handled = page.step(
            &ScreenEvent::FocusMoved {
                from: None,
                to: FocusKey { entry: EntryId(1), elem: BAND },
                by: crate::ui::screen::By::Pointer,
            },
            &c,
            &mut mk_fx(&mut out, &mut present),
        );
        assert_eq!(handled, Handled::Yes);
        assert_eq!(draft_before, page.draft, "a hover over the band must not answer anything");
        assert!(
            !out.iter().any(|s| matches!(&s.fx, Fx::Nav(_) | Fx::App(_))),
            "…and must not navigate or record anything either"
        );

        restore_consent_snapshot(saved);
    }

    /// **Toggling a switch must ask the engine for NOTHING.** Reported: "focus immediately jumps
    /// to Done". Legacy's `on_ok` wrote `ACTION_FOCUSED = true` on every flip; the honest
    /// re-proof now that focus lives entirely in the shared `FocusEngine` (never in a field this
    /// screen owns) is that `row_commit`'s switch arms emit no `Fx::Deliver(.., Enter(..))` at
    /// all — the ONLY channel this page has for moving focus anywhere (see `request_band_focus`
    /// and the Delete row's own re-seat, above). With nothing asked for, the outer engine's
    /// current key is left exactly where it was: on the row that was toggled. A regression that
    /// reintroduces "ask for the band on every flip" would fail this immediately; a regression
    /// that moved focus through some OTHER channel this screen does not have yet would not be
    /// caught here — see the composed module below for what IS provable about the engine's own
    /// answer to a direction key.
    #[test]
    fn toggling_a_switch_keeps_focus_on_the_row_it_toggled() {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        consent::install(Consent::default());
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));

        out.clear();
        let handled = page.step(&ScreenEvent::Activate(0), &c, &mut mk_fx(&mut out, &mut present));
        assert_eq!(handled, Handled::Yes, "OK on the Crash reports switch");
        assert_eq!(page.draft, (true, false), "…flips exactly that switch");
        assert!(
            !out.iter().any(|s| matches!(&s.fx, Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(_))))),
            "a switch flip must ask the engine for NOTHING — not the band, not anywhere else; \
             whatever it asked for is where focus would jump to next, and the legacy bug asked \
             for Done explicitly"
        );

        restore_consent_snapshot(saved);
    }

    /// **No edit may leave focus on nothing.** Reported: "toggling the value back can cause
    /// focus to disappear completely". Legacy parked focus on Done when a value first changed,
    /// then removing Done (toggling back to the stored answer) left nothing to hold that focus —
    /// the screen had no ring anywhere while the keys still moved an invisible selection. The
    /// fix above (no re-seat request on EITHER toggle direction) makes the failure mode
    /// structurally unreachable rather than merely patched: there is no re-seat request this
    /// screen could have made that targets Done, so there is nothing for Done's disappearance to
    /// strand. This test is the same toggle-and-reverse the report describes, watching for the
    /// same absence on the way back down as the test above watches for on the way up.
    #[test]
    fn toggling_a_value_back_never_leaves_focus_on_nothing() {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        consent::install(Consent::default());
        let m = FixtureMeasure;
        let c = test_cx(&m);
        let (mut out, mut present) = sink();
        let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));

        page.row_commit(0, &mut mk_fx(&mut out, &mut present)); // Crash reports -> On, Done appears
        assert_eq!(page.draft, (true, false));
        assert_eq!(page.band_labels().len(), 1, "Done is now on screen");

        out.clear();
        page.row_commit(0, &mut mk_fx(&mut out, &mut present)); // …and back Off again
        assert_eq!(page.draft, (false, false), "the draft matches the stored answer again");
        assert!(page.band_labels().is_empty(), "…and Done goes away with it");
        assert!(
            !out.iter().any(|s| matches!(&s.fx, Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(_))))),
            "removing Done must not ask the engine to re-seat anywhere either — with nothing \
             requested on either edit, the engine's own current key never left the table row \
             being toggled, so there is no Done for it to have been stranded on"
        );

        restore_consent_snapshot(saved);
    }

    /// **Focus navigation, driven through the REAL shared engine — the level a navigation
    /// regression pin now has to be re-proven at.** Every one of legacy's `on_left_right`/
    /// `on_updown` bugs lived in a screen-local FSM that hand-rolled the walk between the answer
    /// band and the reading list; that FSM is gone, and the walk is now `ui/focus.rs`'s
    /// `FocusEngine`, reached through this page's `Focusable` view exactly as `ui/dispatch.rs`'s
    /// `after_step` reaches it once a screen declines a directional key itself (§7.3 steps 2-5).
    ///
    /// Driving the walk through a full `Dispatcher` + `RouteSurface` (the shape
    /// `screens::settings`'s own `mod composed` stands up, since a Settings-family page never
    /// owns a surface of its own) would need this file to name `screens::settings` — the one
    /// thing `screens/mod.rs`'s layer rule ("a screen names `ui/`, `stores/`, the data crates and
    /// this directory's `registry`; never … a sibling screen module") exists to forbid, and the
    /// one file this audit does not own. So this module drives the engine directly against
    /// `ConsentPage::view()` instead: the same query protocol (`groups`/`neighbour`/`place`/
    /// `reconcile`/`seat`), the same edge rules (`ui/table_screen.rs`'s `TablePart`/`BandPart`),
    /// and the same answer a real key press would get — one layer shorter than the composed
    /// harness, but the REAL mechanism rather than a restatement of it.
    mod composed {
        use super::*;
        use crate::ui::focus::{FocusEngine, Outcome};
        use crate::ui::screen::{By, Dir};

        /// One direction, through the real engine, against `page`'s live view — the composed
        /// twin of `page.step(...)` for the half of the focus protocol a screen's own `step`
        /// never answers.
        fn go(engine: &mut FocusEngine<u32>, owner: InputOwner, page: &ConsentPage, dir: Dir, c: &Cx<'_, InnerHost>) -> Outcome<u32> {
            engine.move_dir(owner, &page.view(), &[], dir, c)
        }

        /// **The second half of this audit's LEFT-escape defect, and the RIGHT half legacy never
        /// implemented at all.** Reported against Share Crash Reports: "focus cannot navigate
        /// correctly from the bottom buttons back to the options on the right." `on_left_right`
        /// answered LEFT/RIGHT only as a walk between the two answers and stopped dead at either
        /// end (rule 7 never reached the reading list from the trailing control). `BandPart`'s
        /// own RIGHT edge is `Geometric` unconditionally now — the same rule every family band
        /// uses — and `Seat::Remembered` on the band's own group is what makes the round trip
        /// back in land on the control that was actually left, not a hardcoded leading one.
        #[test]
        fn right_off_the_trailing_answer_reaches_the_reading_list() {
            let m = FixtureMeasure;
            let c = test_cx(&m);
            let (mut out, mut present) = sink();
            let page = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
            let mut engine: FocusEngine<u32> = FocusEngine::new();
            let owner = InputOwner::Entry(EntryId(1));
            // First run's own mount asks the outer engine to open on the band's first control
            // (asserted separately by `first_run_requests_band_focus_on_its_own_mount`, above);
            // seed that premise here rather than re-deriving it, so this test is about the WALK.
            engine.set(owner, FocusKey { entry: EntryId(1), elem: BAND }, Some(BAND_GROUP), By::Dir);

            let outcome = go(&mut engine, owner, &page, Dir::Right, &c);
            assert!(matches!(outcome, Outcome::Moved { .. }), "RIGHT walks to the second answer: {outcome:?}");
            assert_eq!(engine.current(owner), Some(FocusKey { entry: EntryId(1), elem: BAND + 1 }));

            let outcome = go(&mut engine, owner, &page, Dir::Right, &c);
            let after = engine.current(owner).expect("focus is still seated somewhere");
            assert!(
                band_index(after.elem).is_none(),
                "RIGHT off the trailing answer must reach the list, not wrap or dead-end: {outcome:?}"
            );

            let outcome = go(&mut engine, owner, &page, Dir::Left, &c);
            assert!(matches!(outcome, Outcome::Moved { .. }), "LEFT walks back in: {outcome:?}");
            assert_eq!(
                engine.current(owner),
                Some(FocusKey { entry: EntryId(1), elem: BAND + 1 }),
                "…trailing control first, so the round trip is exact"
            );
        }

        /// **Reported: "from Done, Right does not return to the table, while Up strangely
        /// does."** `on_left_right` only ever answered for first run, so in Settings mode LEFT
        /// and RIGHT were both dropped on the floor and the band was a rightward dead end (rules
        /// 5 and 7). `TablePart`'s LEFT edge is `Geometric` whenever a band exists (rule 5) and
        /// `BandPart`'s RIGHT edge is `Geometric` unconditionally (rule 7) — both generic, both
        /// exercised here against this screen's real groups (a band only exists once a value
        /// differs from what is stored, so the test toggles one first, through `row_commit`
        /// exactly as a real OK on the switch would).
        #[test]
        fn left_reaches_the_action_band_and_right_comes_back_out_of_it() {
            let _g = crate::testlock::serial();
            let saved = consent::current();
            consent::install(Consent::default());
            let m = FixtureMeasure;
            let c = test_cx(&m);
            let (mut out, mut present) = sink();
            let mut page = ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present));
            page.row_commit(0, &mut mk_fx(&mut out, &mut present)); // Crash reports -> On
            assert_eq!(page.band_labels().len(), 1, "Done is on screen once a value differs");

            let mut engine: FocusEngine<u32> = FocusEngine::new();
            let owner = InputOwner::Entry(EntryId(1));
            engine.set(owner, FocusKey { entry: EntryId(1), elem: 0 }, Some(TABLE_GROUP), By::Dir);

            let outcome = go(&mut engine, owner, &page, Dir::Left, &c);
            assert!(matches!(outcome, Outcome::Moved { .. }), "LEFT from the list reaches the band: {outcome:?}");
            assert_eq!(
                engine.current(owner),
                Some(FocusKey { entry: EntryId(1), elem: BAND }),
                "…landing on Done, the band's only control"
            );

            let outcome = go(&mut engine, owner, &page, Dir::Right, &c);
            assert!(matches!(outcome, Outcome::Moved { .. }), "RIGHT from the band returns to the list: {outcome:?}");
            assert_eq!(
                engine.current(owner),
                Some(FocusKey { entry: EntryId(1), elem: 0 }),
                "…back on the row it left — `Seat::Remembered` on the table's own group, not just ANY row"
            );

            restore_consent_snapshot(saved);
        }

        /// UP leaves the band for the list and DOWN off the last row comes back — one vertical
        /// relationship, shared by both `TablePart`'s and `BandPart`'s own `Geometric` up/down
        /// edges, so the band is never a dead end from either side.
        #[test]
        fn the_answer_row_and_the_reading_list_are_one_vertical_walk() {
            let m = FixtureMeasure;
            let c = test_cx(&m);
            let (mut out, mut present) = sink();
            let page = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
            let mut engine: FocusEngine<u32> = FocusEngine::new();
            let owner = InputOwner::Entry(EntryId(1));
            engine.set(owner, FocusKey { entry: EntryId(1), elem: BAND }, Some(BAND_GROUP), By::Dir);

            let outcome = go(&mut engine, owner, &page, Dir::Up, &c);
            assert!(matches!(outcome, Outcome::Moved { .. }), "UP leaves the band for the list: {outcome:?}");
            let after = engine.current(owner).expect("focus landed on the list");
            assert!(band_index(after.elem).is_none());

            // Seat on the list's LAST row before asking DOWN off it: the property under test is
            // that the LAST row (not the first) returns to the band.
            let last_row = page.table.n_rows() - 1;
            engine.set(owner, FocusKey { entry: EntryId(1), elem: last_row as u32 }, Some(TABLE_GROUP), By::Dir);
            let outcome = go(&mut engine, owner, &page, Dir::Down, &c);
            assert!(matches!(outcome, Outcome::Moved { .. }), "DOWN off the last row returns to the answers: {outcome:?}");
            assert_eq!(
                band_index(engine.current(owner).unwrap().elem),
                Some(0),
                "…landing on the band"
            );
        }
    }
}
