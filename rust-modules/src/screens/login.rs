//! **The sign-in screen, as an owned `Screen`** (restructure spec §13, phase 6 — `ui/login.rs`
//! moved). Plex's own server-rendered QR PNG (fetched by [`crate::auth`], decoded + tinted here)
//! plus the typed short-code fallback, driven by the flow's phase. Scanning the QR on a phone
//! opens plex.tv pre-filled with the pin; the flow's background poll then advances us onward.
//!
//! It has no panel of its own and is not a table. Its controls are ONE group of up to two
//! `ElemKind::Bare` elements, walked in order: the read-out's primary action (the failed/stalled/
//! deleted pill, or — while the code is simply unscanned for a long time — the QR screen's own
//! "press OK for a new code" sentence), and, while a sign-in failure is held as an onboarding
//! incident, *Details*. Both fire on the OK key-down edge with no hold and no press bounce,
//! exactly as the lone action did under the old `key()` ladder.
//!
//! **The onboarding report.** Session raises the failure as an incident (`auth::owner::incident`);
//! this screen is the one that shows it, so it is the one that resolves it against the report
//! permission it reads now ([`auth::SessionCmd::ResolveIncident`]). An undetermined permission
//! turns into a question — a [`DecisionAlert`] over the read-out, *Not now* / *Send report* — and
//! a decided one never asks: a Yes at the onboarding scope is sent by Session without a press, a
//! No keeps nothing. A stalled QR wait never asks at all — Session resolves it to `OnRequest`,
//! so the code stays uncovered and only *Details* offers it. Whatever the answer, *Details* keeps
//! the Report ID, the support line and *Send report* one press away for as long as the failure is
//! on screen.
//!
//! **Details is a card, never an expansion** (owner, 2026-09-19: "3 buttons and labels. Looks like
//! a mess."). The read-out itself never grows: *Details* opens the same [`DecisionAlert`] the
//! question uses, titled "Details", whose body is the Report ID (once there is one) and the
//! support line, and whose answers are *Close* and — only while a report can still be sent —
//! *Send report*, which starts focused when it is there on open. While the card stays open its
//! content follows the incident, retaining valid focus as receipts and answers change. BACK or *Close* puts focus back on
//! *Details*; *Send report* closes the card too. Ordinary failure read-outs then show
//! "Sending report…" beside its spinner; helper save warnings keep their storage stage visible. The QR screen's *Details* opens the same card.
//!
//! **The calm default.** Until somebody acts (or a standing Yes sends one), the failure is the
//! design system's `StatusOverlay` failed and nothing else: verdict, reason, *Try again* /
//! *Details*. A report adds at most ONE short status line under the row ([`report_status`]); a
//! helper save warning uses that line for its photographable storage stage. The
//! Report ID lives only inside the Details card. A report still on its way carries the shared inline
//! spinner beside "Sending report…" wherever that line is drawn — the QR screen's stall line keeps
//! its own spinner too.
//!
//! The constructor and each `Tick` consume one immutable [`auth::SessionRead`] publication through
//! [`AuthLike`]. Its login client-id capture resolves on the storage worker when the visible
//! cache is empty (including local revocation), so a transient peek is never latched as the ID.
//! The QR code, bitmap and generation therefore come from one retained snapshot; draw,
//! prepare and focus geometry read only the screen's cached fields. Commands travel as typed
//! [`auth::SessionCmd`] effects. A stalled-wait clock is reset only by its matching accepted
//! [`AppMsg::RestartReply`], never when the request is merely emitted.

use std::ffi::{CStr, CString};
use std::os::raw::c_int;
use std::sync::Arc;

use crate::auth::{self, Phase};
use crate::ui::frame::Budget;
use crate::ui::label::HAlign;
use crate::ui::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, Fx, GroupId, Handled, InputEvent, InputKind, Key,
    LogicalState, Machine, Measure, Tick,
};
use crate::ui::route_screen::{RouteGround, RouteLayout};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusSource, FocusTarget,
    Focusable, GroupKind, GroupSpec, HitSource, Hover, Placed, RenderStrategy, Screen, ScreenEvent,
    Seat, Step, Stop,
};
use crate::ui::text_view::TextView;
use crate::ui::decision_alert::{Choice, DecisionAlert, Tone};
use crate::ui::widgets::{Button, CtlPop, Spinner, StatusKind, StatusOverlay};
use crate::ui::{theme, Env, Painter, Rect, View};

use super::plaintext_question::{self, PlaintextQuestion, CONNECT};
use super::registry::{word, AppFx, AppLike, AppMsg, AuthLike};

/// The screen's elements. The read-out's primary action and *Details* share [`CONTROL_GROUP`];
/// the alert's answers — the report question's *Not now* / *Send report*, the Details card's
/// *Close* / *Send report* — are [`ALERT_GROUP`], the only group while the alert is open. `GroupId(0)` matches the container's own default fresh-mount target
/// (`stack.rs::fresh`), which is what lets a screen that mounts straight into `Phase::Deleted` (a
/// real control from frame one) get seated by the ordinary Mount → Enter sequence with no
/// correction of its own.
const CONTROL: u32 = 0;
const DETAILS: u32 = 1;
/// The alert's cancel slot: *Not now* on the question, *Close* on the Details card.
const ALERT_CANCEL: u32 = 3;
const ALERT_SEND: u32 = 4;
const CONTROL_GROUP: GroupId = GroupId(0);
const ALERT_GROUP: GroupId = GroupId(1);

const DETAILS_LABEL: &CStr = c"Details";
/// The Details card's title — the pill's own word, so the card names what opened it.
const DETAILS_TITLE: &CStr = c"Details";
const SEND_REPORT: &CStr = c"Send report";
const NOT_NOW: &CStr = c"Not now";
const CLOSE: &CStr = c"Close";
const REPORT_QUESTION: &CStr = c"Send a report about this sign-in problem?";
/// The report alert's disclosure — short, because it is read from the sofa. **Every clause is a
/// claim about `telemetry::incident::event_body`** and must stay true of it: "which sign-in step
/// failed and how the connection answered" is the `incident` context (kind, link class, HTTP
/// status or `CURLcode`, the bucketed counters, the code generation), "the app version" is
/// `release`; and every category it rules out — the same five `PRIVACY.md`'s onboarding section
/// names — is not in the body at all. The full field list lives in `PRIVACY.md` and the in-app
/// Privacy Policy, not here.
pub(crate) const REPORT_BODY: &str = "The report says which sign-in step failed and how the \
connection answered, storage failure stages and error numbers, plus the app version. It never includes your account name, tokens, PIN, \
sign-in code or network addresses.";

/// How long a working phase runs before the read-out grows a way out.
///
/// **Not zero**: a healthy LAN discovery finishes in well under a second, and a control that
/// flashes past on every sign-in is noise that teaches people to ignore it. **Not longer**: from
/// the sofa a spinner that will never stop looks exactly like one that is about to, and until this
/// existed there was no way at all out of a wedged sign-in — BACK is the root press (see
/// `Machine::step`'s BACK arm), and on a first-ever boot there is no stored session for it to
/// resume, so the only exit was killing the app.
const ESCAPE_AFTER_MS: f32 = 12_000.0;

/// Whether a wait has run long enough to be worth offering an escape from.
///
/// Pure and separate from the draw so the threshold is gradeable on the host.
fn escape_offered(phase_ms: f32) -> bool {
    phase_ms >= ESCAPE_AFTER_MS
}

/// The phases that are genuinely WAITING ON A NETWORK CALL and can therefore stall.
///
/// **An allowlist, not "everything that is not terminal".** It was the latter for an hour, which
/// swept in `Ready`, `Profiles` and `Switching` — phases the main loop routes away from on its next
/// pass. A stalled-wait control answered on one of those would call `auth::retry`/
/// `auth::restart_stalled_wait` on a flow that had already SUCCEEDED, replacing a completed handoff
/// with a fresh sign-in. `Idle` is excluded for the same reason from the other side: nothing is
/// owed, so there is nothing to retry.
fn working_phase(phase: Phase) -> bool {
    matches!(phase, Phase::Creating | Phase::Discovering)
}

/// The verb on both the failed and the stuck read-out, because it is the same call underneath.
///
/// `auth::retry`/`auth::restart_stalled_wait` bump the auth epoch, so a worker still blocked in the
/// wedged request has its result discarded when it finally returns, and it re-runs only the leg
/// that failed — discovery when the pin already yielded an account credential, a whole fresh pin
/// when it did not.
const ESCAPE: &CStr = c"Try again";
const SIGN_IN: &CStr = c"Sign in";
/// AUTH-03: acknowledges a fresh save the disk could not confirm — proceed, knowing the next
/// launch may ask you to sign in again.
const CONTINUE_UNSAVED: &CStr = c"Continue";

/// How long a QR code may go unscanned before the screen offers to replace it on request.
///
/// **A separate, much longer clock than [`ESCAPE_AFTER_MS`], because this wait is not a stall.**
/// Twelve seconds is right for a spinner that should have finished in one; a code on screen is
/// waiting for a person to find their phone, unlock it, open a camera and tap a link, and nagging
/// them at twelve seconds would be wrong every time. A full minute of a code that has already been
/// scanned is not.
///
/// It exists because the automatic replacement (a pin that runs out is re-minted automatically)
/// cannot cover the case the issue reported: the phone says *Account linked* while our polls are
/// being answered `Pending` or nothing at all, and the person watching knows something the
/// television does not. Waiting out the rest of a fifteen-minute lease is not a recovery.
const QR_ESCAPE_AFTER_MS: f32 = 60_000.0;

/// Whether the QR screen is offering its own replacement right now. Pure, and — like
/// [`escape_offered`] — the ONE predicate behind both the sentence and the key, so a control that
/// is not drawn can never be activated.
fn qr_escape_offered(phase_ms: f32) -> bool {
    phase_ms >= QR_ESCAPE_AFTER_MS
}

/// **What the screen is waiting ON**, as the pair [`LoginScreen::phase_ms`] is timing.
///
/// The phase alone was the whole identity while a code could only change by leaving `Waiting`. It
/// cannot be any more: a pin that runs out is replaced automatically, `Waiting → Creating →
/// Waiting`, and a `Tick` samples once a frame — so a replacement completed between two samples (a
/// paused main loop, a long frame) is invisible, and the FRESH code inherits the dead one's age. It
/// would then offer "press OK for a new code" about a code that had existed for a millisecond.
/// Including the generation makes the reset exact rather than probable.
type Wait = (Phase, u64);

/// Is what the screen is waiting on a DIFFERENT thing from what it was waiting on last frame?
///
/// Trivial, and separate anyway, because the rule it encodes is not: a new CODE restarts the clock
/// exactly as a new PHASE does, and the version that compared phases alone is the one that would
/// offer to replace a code a millisecond old.
fn wait_restarted(seen: Wait, live: Wait) -> bool {
    seen != live
}

/// Release the cached QR texture as soon as it stops describing the code the flow is showing.
///
/// **Keyed on the QR generation, not on the phase, and that swap is this screen's half of issue
/// #30.** The rule used to be "a retry enters `Creating`, so drop it there" — which was true of the
/// only way a code could ever change. It no longer is: a pin that runs out is now replaced
/// automatically, and the flow returns to the same `Waiting` it was already in. A cache keyed on
/// the phase would have gone on drawing the dead code — sharp, scannable, and pointing at a pin
/// plex.tv had forgotten — for the whole of its successor's life. `Creating` is still checked, as
/// the belt to the generation's braces: it is the one moment a flow is known to have thrown its
/// code away before any replacement exists.
fn qr_cache_stale(cached: u64, live: u64, phase: Phase) -> bool {
    cached != live || phase == Phase::Creating
}

/// What the delete actually achieved, as the two lines it is honest to draw.
///
/// **A partial wipe may not be reported as a whole one**, and that is not pedantry: the files this
/// sweep can fail on include the TELEMETRY decision, so a survivor is re-read on the next launch
/// and a consent the user believed they had deleted comes back. The session is gone either way, so
/// the verdict stays true and the reason carries the qualification.
fn deleted_readout(leftovers: usize) -> (&'static CStr, &'static CStr) {
    if leftovers == 0 {
        (
            c"Local data deleted",
            c"Credentials, preferences, telemetry and local diagnostics have been removed.",
        )
    } else {
        (
            c"Signed out, and most local data deleted",
            c"Some files could not be removed and may still be on this television.",
        )
    }
}

/// The line under the code, which has to answer a question that only exists now that a code can be
/// replaced: *why is this not the code I was looking at*.
///
/// A pin lives fifteen minutes and is re-minted when it runs out, so somebody who walked away
/// mid-sign-in — or whose phone has just told them the OLD code was linked — comes back to
/// different digits. Saying nothing there reads as the television having lost track of itself, and
/// it is exactly the moment they need to be told to scan again. **`stalled` outranks
/// `code_replaced`**: one of these sentences carries an ACTION, and a line that explains history is
/// worth less than the one that offers a way forward.
///
/// **`unreachable` outranks both.** While plex.tv is not answering at all, a scan cannot complete
/// and a new code cannot be issued, so neither of the other sentences is true; the one useful
/// thing to say is where the fault most likely is.
fn waiting_status(code_replaced: bool, stalled: bool, unreachable: bool) -> &'static CStr {
    if unreachable {
        c"Can\u{2019}t reach Plex. Check your TV\u{2019}s internet connection."
    } else if stalled {
        c"Still waiting — press OK for a new code"
    } else if code_replaced {
        c"That code expired — scan this one"
    } else {
        c"Waiting for you to sign in…"
    }
}

/// The complete manual-link stack in the content column.
///
/// The QR itself is centred on the television's Y axis as requested, while the URL, manual code
/// and waiting state flow from its edges. That makes the scan target visually central without
/// separating the fallback credentials into unrelated screen coordinates.
#[derive(Clone, Copy)]
struct QrLayout {
    url: Rect,
    card: Rect,
    code: Rect,
    status: Rect,
}

fn qr_layout(layout: RouteLayout) -> QrLayout {
    const SIDE: f32 = 420.0;
    let card = Rect::new(
        layout.content.cx() - SIDE * 0.5,
        Rect::FULL.cy() - SIDE * 0.5,
        SIDE,
        SIDE,
    );
    let url_h = theme::size::TITLE as f32 + theme::space::XS;
    let code_h = theme::size::DISPLAY as f32 + theme::space::XS;
    let status_h = theme::size::BODY as f32 + theme::space::SM;
    QrLayout {
        url: Rect::new(
            layout.content.x,
            card.y - theme::space::LG - url_h,
            layout.content.w,
            url_h,
        ),
        card,
        code: Rect::new(
            layout.content.x,
            card.y + card.h + theme::space::LG,
            layout.content.w,
            code_h,
        ),
        status: Rect::new(
            layout.content.x,
            card.y + card.h + theme::space::LG + code_h + theme::space::MD,
            layout.content.w,
            status_h,
        ),
    }
}

/// **The read-out, built ONCE for both its uses** — the draw, and the geometry the focus engine and
/// the hit test read ([`status_row_rects`]), so the two can never disagree about where a control
/// is (`ui/table_screen.rs`'s rule: "the DRAW reads the same formula"). `labels` is the row by
/// slot — the primary, then *Details* — and `note` the report's one quiet status line under it.
/// It fills the page, so a `Failed` one hangs from `StatusOverlay::FULL_ANCHOR_TOP` through
/// `.page()`, the same as Home's and the Library's.
fn readout_overlay<'a>(
    caption: &'a CStr,
    kind: StatusKind,
    reason: Option<&'a CStr>,
    labels: [Option<&'a CStr>; 2],
    note: Option<&'a Note>,
) -> StatusOverlay<'a> {
    let mut o = StatusOverlay::new(Rect::FULL, caption, kind).page();
    if let Some(r) = reason {
        o = o.reason(r);
    }
    if let Some(note) = note {
        o = o.note(Some(note.text.as_c_str())).note_busy(note.busy);
    }
    if let Some(primary) = labels[0] {
        o = o.action(primary).secondary(labels[1]);
    }
    o
}

/// The read-out's controls, placed by the WIDGET through the [`Measure`] capability — the same
/// `StatusOverlay::action_frames_measured` its draw uses. The overlay built here carries no
/// caption or reason TEXT: the row moves with the KIND (a `Working` caption sits lower, a
/// page-filling `Failed` one stands on the shared page lines) and with whether a reason exists (a
/// `Failed` reason is a two-line slot whatever it says).
fn status_row_rects(
    measure: &dyn Measure,
    labels: [Option<&CStr>; 2],
    kind: StatusKind,
    has_reason: bool,
) -> [Option<Rect>; 2] {
    readout_overlay(c"", kind, has_reason.then_some(c""), labels, None).action_frames_measured(measure)
}

/// The lone action's rect — [`status_row_rects`] for a read-out with one control.
#[cfg(test)]
fn status_action_rect(
    measure: &dyn Measure,
    label: &CStr,
    kind: StatusKind,
    has_reason: bool,
) -> Rect {
    status_row_rects(measure, [Some(label), None], kind, has_reason)[0].unwrap_or(Rect::FULL)
}

/// What the one control (when it exists at all) does when pressed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ControlKind {
    /// The stalled-wait escape — either the Working spinner's `Try again` (12 s) or the QR
    /// screen's `press OK for a new code` (60 s). Both restart the SAME wait this screen has been
    /// timing (`auth::restart_stalled_wait`), which is why they share one verb here even though
    /// they read different sentences on screen.
    RestartWait,
    /// `Phase::Error`'s `Try again` — acts unconditionally; there is no live worker to race.
    Retry,
    /// `Phase::Deleted`'s `Sign in` — starts a whole fresh flow.
    StartLogin,
    /// AUTH-03: acknowledges an unconfirmed fresh save and releases the held Ready handoff (or,
    /// for a Discovery-site warning, re-admits the final commit that produces one).
    ContinueUnsaved,
    /// `Phase::Error` for an eligible plaintext-only server not yet answered
    /// (`plaintext_question::asks`): `Connect` asks the consent question ([`Sheet::Plaintext`]) —
    /// the read-out keeps one primary, never a third button. Once answered the primary is *Try
    /// again* ([`ControlKind::Retry`]), and the reason says what it does.
    ConnectPlaintext,
}

fn label_for(kind: ControlKind) -> &'static CStr {
    match kind {
        ControlKind::RestartWait | ControlKind::Retry => ESCAPE,
        ControlKind::StartLogin => SIGN_IN,
        ControlKind::ContinueUnsaved => CONTINUE_UNSAVED,
        ControlKind::ConnectPlaintext => CONNECT,
    }
}

/// Every branch but the two SETTLED read-outs (`Failed`/`Deleted`) draws the spinner — the one
/// thing on this screen that animates from a raw clock (`spin_ms`) rather than a spring `ui::idle`
/// can see on its own. `Spinner::draw`'s own module note is the standing warning that this class of
/// animator ships FROZEN if it forgets to report every frame it is on screen.
fn control_has_spinner(phase: Phase, warning_showing: bool) -> bool {
    !warning_showing && !matches!(phase, Phase::Error | Phase::Deleted)
}

fn phase_disc(p: Phase) -> u8 {
    match p {
        Phase::Idle => 0,
        Phase::Creating => 1,
        Phase::Waiting => 2,
        Phase::Discovering => 3,
        Phase::Profiles => 4,
        Phase::Switching => 5,
        Phase::Ready => 6,
        Phase::Error => 7,
        Phase::Deleted => 8,
    }
}

struct LoginState {
    phase: u8,
    qr_gen: u64,
    qr_replaced: bool,
    has_control: bool,
    delete_leftovers: u32,
    next_correlation: Option<u32>,
    pending_restart: Option<u32>,
    /// AUTH-03: whether a `PersistenceWarning` is currently shown. The key itself is not part of
    /// the logical state a container needs to notice a change — only whether one is showing.
    warning: bool,
    report: ReportState,
    /// `Some(question_open)` while the read-out offers an unencrypted connection. Written to the
    /// canon only when `Some`, so every other state's digest is unchanged.
    plaintext: Option<bool>,
}

/// The onboarding report's part of the logical state: which offer is shown and where it has got
/// to, the disclosure, the alert, and the last resolution this screen asked for.
#[derive(Clone, Copy, Default)]
struct ReportState {
    offer: Option<(u32, u8)>,
    link_trouble: bool,
    /// the Details card (rather than the report question) is the open alert
    details_open: bool,
    /// `Some(send_focused)` while the alert is open
    alert: Option<bool>,
    resolved: Option<(u32, u32)>,
}

fn incident_state_disc(state: &auth::owner::IncidentState) -> u8 {
    use auth::owner::IncidentState as S;
    match state {
        S::Pending => 0,
        S::Offered { .. } => 1,
        S::AutoSending => 2,
        S::Sending => 3,
        S::Queued { .. } => 4,
        S::Saved { .. } => 9,
        S::Delivered { .. } => 10,
        S::Failed => 5,
        S::NotNow => 6,
        S::Dropped => 7,
        S::OnRequest { .. } => 8,
    }
}

impl LogicalState for LoginState {
    fn write(&self, w: &mut Canon) {
        w.u8(self.phase);
        w.u64(self.qr_gen);
        w.bool(self.qr_replaced);
        w.bool(self.has_control);
        w.u32(self.delete_leftovers);
        w.option(self.next_correlation, |w, correlation| {
            w.u32(correlation);
        });
        w.option(self.pending_restart, |w, correlation| {
            w.u32(correlation);
        });
        w.bool(self.warning);
        let r = &self.report;
        w.option(r.offer, |w, (id, state)| {
            w.u32(id).u8(state);
        });
        w.bool(r.link_trouble).bool(r.details_open);
        w.option(r.alert, |w, send| {
            w.bool(send);
        });
        w.option(r.resolved, |w, (id, revision)| {
            w.u32(id).u32(revision);
        });
        if let Some(open) = self.plaintext {
            w.bool(open);
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "login phase={} qr_gen={} replaced={} control={} leftovers={} next={:?} restart={:?} warning={} \
             incident={:?} link_trouble={} details={} alert={:?} resolved={:?} plaintext={:?}",
            self.phase,
            self.qr_gen,
            self.qr_replaced,
            self.has_control,
            self.delete_leftovers,
            self.next_correlation,
            self.pending_restart,
            self.warning,
            self.report.offer,
            self.report.link_trouble,
            self.report.details_open,
            self.report.alert,
            self.report.resolved,
            self.plaintext,
        ));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct PendingRestart {
    correlation: u32,
    wait: Wait,
}

/// Everything the screen keeps for the onboarding report — see the module doc.
struct Report {
    /// The incident Session is holding, retained from the publication.
    offer: Option<auth::owner::IncidentOffer>,
    /// Session's "plex.tv is not answering the wait" — the QR screen's status line.
    link_trouble: bool,
    /// Which card the alert is showing — see [`Sheet`].
    sheet: Sheet,
    /// The support line for [`Self::support_for`]'s offer: product, version, firmware, set and
    /// the failure's code — built once per offer, since the firmware and set cannot change.
    support: CString,
    support_for: Option<u32>,
    alert: DecisionAlert,
    /// The offer the alert was opened for. Set once per offer, so an answered offer is not asked
    /// again when its state flickers back.
    alert_for: Option<u32>,
    /// The alert's answers as last drawn — `Focusable` answers with `&self`.
    alert_frames: Option<(Rect, Rect)>,
    /// The last `(id, revision)` this screen resolved, so a pending offer is resolved once rather
    /// than on every frame the owner has yet to answer.
    last_resolve: Option<(u32, u32)>,
    /// The control group's focus pops, by slot (primary, Details).
    pop: CtlPop<2>,
}

/// What the one [`DecisionAlert`] is showing: the report QUESTION the screen asks once per offer,
/// or the DETAILS card a press on *Details* opens. Their answers share the alert's two slots — the
/// cancel slot is *Not now* / *Close*, the second *Send report* on both.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sheet {
    Question,
    Details,
    /// "Connect without encryption?" — *Not now* / *Connect*, asked from the read-out's primary.
    Plaintext,
}

impl Report {
    fn new() -> Self {
        let mut alert = DecisionAlert::new();
        alert.set_tone(Tone::Neutral);
        Self {
            offer: None,
            link_trouble: false,
            sheet: Sheet::Question,
            support: CString::default(),
            support_for: None,
            alert,
            alert_for: None,
            alert_frames: None,
            last_resolve: None,
            pop: CtlPop::new(),
        }
    }

    fn state(&self) -> ReportState {
        ReportState {
            offer: self.offer.as_ref().map(|o| (o.id, incident_state_disc(&o.state))),
            link_trouble: self.link_trouble,
            details_open: self.alert.is_open() && self.sheet == Sheet::Details,
            alert: self.alert.is_open().then(|| self.alert.choice() == Choice::Destructive),
            resolved: self.last_resolve,
        }
    }

    /// The report's one quiet status line — see [`report_status`].
    fn status(&self) -> Option<(&'static CStr, bool)> {
        report_status(&self.offer.as_ref()?.state)
    }

    /// The Details card's body: the Report ID line once there is one, then the support line.
    fn details_body(&self) -> Vec<std::borrow::Cow<'static, str>> {
        let mut out = Vec::new();
        if let Some(r) = self.receipt() {
            out.push(report_id_line(r).into());
        }
        if !self.support.is_empty() {
            out.push(self.support.to_string_lossy().into_owned().into());
        }
        out
    }

    /// Whether a press on *Send report* would be accepted now.
    fn sendable(&self) -> bool {
        self.offer.as_ref().is_some_and(|o| o.sendable())
    }

    /// The Report ID a person can quote, once the report has one.
    fn receipt(&self) -> Option<&str> {
        use auth::owner::IncidentState as S;
        match &self.offer.as_ref()?.state {
            S::Queued { receipt } | S::Saved { receipt } | S::Delivered { receipt } => Some(receipt),
            _ => None,
        }
    }
}

/// **The one short line that says what became of a report**, and whether it is still on its way
/// (the one fact that can put a spinner beside it). `None` until somebody has acted or a standing
/// Yes sent one: a failure nobody has reported says nothing about reporting at all. "Sent" is kept
/// for a server's acceptance; a queued report is still being sent. The Report ID is never on this
/// line — it lives inside the Details card ([`report_id_line`]).
fn report_status(state: &auth::owner::IncidentState) -> Option<(&'static CStr, bool)> {
    use auth::owner::IncidentState as S;
    Some(match state {
        S::Sending | S::AutoSending | S::Queued { .. } => (c"Sending report\u{2026}", true),
        S::Delivered { .. } => (c"Report sent. Thank you.", false),
        S::Saved { .. } => (c"Report saved. It will be sent later.", false),
        S::Failed => (c"Report couldn\u{2019}t be sent.", false),
        S::Pending | S::Offered { .. } | S::OnRequest { .. } | S::NotNow | S::Dropped => return None,
    })
}

/// A Report ID as a person reads it aloud: lowercase hex in groups of four
/// (`41de 4cd3 88e4 0416 54de 38f2 787c 3922`). The id's own separators (a UUID's hyphens) are
/// dropped first, so the grouping is the only spacing in it.
fn group_report_id(receipt: &str) -> String {
    let chars: Vec<char> = receipt
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    chars
        .chunks(4)
        .map(|g| g.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The Details card's line that carries the Report ID — labelled, its own paragraph.
fn report_id_line(receipt: &str) -> String {
    format!("Report ID: {}", group_report_id(receipt))
}

/// The report's one status line, and whether a spinner turns beside it.
#[derive(Debug, PartialEq, Eq)]
struct Note {
    text: CString,
    busy: bool,
}

/// The support line a person reads out or photographs: what is running, on which firmware and
/// set, and which failure — codes only, never an address or an account. The trailing segment is
/// [`crate::telemetry::incident::storage_evidence_line`] — the persistence class, key-manager
/// stage and service error code, read from the SAME [`auth::owner::IncidentOffer::context`] this
/// screen's report would send, so a photograph of this line and the report Sentry receives can
/// never disagree about what failed.
fn support_line(offer: &auth::owner::IncidentOffer) -> String {
    use crate::telemetry::incident::LinkClass;
    let set = crate::webos::device().set_line();
    let set = if set.is_empty() { "unknown set".to_string() } else { set };
    let code = match offer.key.link {
        LinkClass::Unknown => offer.key.kind.code().to_string(),
        link => format!("{}.{}", offer.key.kind.code(), link.code()),
    };
    let storage = crate::telemetry::incident::storage_evidence_line(offer.context.as_ref());
    format!(
        "{} {} \u{b7} {} \u{b7} {} \u{b7} {} \u{b7} {}",
        crate::plex::identity::PRODUCT,
        crate::plex::identity::VERSION,
        crate::webos::info().release_line(),
        set,
        code,
        storage
    )
}

/// The control group's elements in walk order. At most two, so a fixed array.
#[derive(Clone, Copy, Default)]
struct Row {
    elems: [u32; 2],
    n: usize,
}

impl Row {
    fn push(&mut self, e: u32) {
        self.elems[self.n] = e;
        self.n += 1;
    }
    fn as_slice(&self) -> &[u32] {
        &self.elems[..self.n]
    }
    fn position(&self, e: u32) -> Option<usize> {
        self.as_slice().iter().position(|&x| x == e)
    }
}

/// The pop/`StatusOverlay` slot an element draws in.
fn slot_of(elem: u32) -> Option<usize> {
    match elem {
        CONTROL => Some(0),
        DETAILS => Some(1),
        _ => None,
    }
}

pub(crate) struct LoginScreen {
    entry: EntryId,
    /// Free-running rotation clock for the spinner, in ms — cached each tick from
    /// [`spin_phase`](Self::spin_phase)'s `advance`. Render-only, never hashed.
    spin_ms: f32,
    /// How long the CURRENT wait has been on screen, in ms — cached each tick from
    /// [`phase_clock`](Self::phase_clock)'s `advance`, reset whenever [`Wait`] changes.
    phase_ms: f32,
    /// The underlying clocks for [`spin_ms`](Self::spin_ms)/[`phase_ms`](Self::phase_ms)
    /// (`motion::Phase` — spec phase 12 D4): an UNBOUNDED clock-driven animator reports `Motion`
    /// from inside its own `advance`, the way [`motion::Ramp`](crate::ui::motion::Ramp) does for a
    /// bounded one, rather than the raw `+= dt` these two fields used to accumulate with
    /// `fx.note(Motion)` called separately, out of band, below.
    spin_phase: crate::ui::motion::Phase,
    phase_clock: crate::ui::motion::Phase,
    wait: Wait,
    /// The uploaded GL texture of Plex's QR PNG (0 until decoded+uploaded) and which generation it
    /// describes. Render resources only — built and freed in [`Screen::prepare`], never in `step`,
    /// so a host test driving `step` alone never touches GL.
    qr_tex: u32,
    qr_tex_gen: u64,
    /// The pixel size that texture was uploaded at, so [`Screen::render_report`] can state the
    /// bytes this screen holds of the frame's render residency (§8.3) instead of guessing them:
    /// the bitmap is whatever plex.tv's PNG decoded to, not a constant. `(0, 0)` while `qr_tex`
    /// is 0, and the two are set and cleared together.
    qr_px: (u32, u32),
    /// PNG bytes `tick` captured this frame, awaiting [`Screen::prepare`]'s decode+upload — the
    /// hand-off between "retain one Session publication" (step) and "touch GL" (prepare). `None` once
    /// consumed, or when nothing new has been published.
    qr_png_pending: Option<(u64, Arc<[u8]>)>,
    /// Everything this screen retains from Session's immutable publication, refreshed once per
    /// `Tick`; `draw` and `prepare` never perform another read.
    phase: Phase,
    qr_gen: u64,
    qr_code: Arc<str>,
    qr_replaced: bool,
    error: Arc<str>,
    delete_leftovers: usize,
    next_correlation: Option<u32>,
    pending_restart: Option<PendingRestart>,
    /// AUTH-03: the warning this screen is answering, if any — synced every `resync` from the
    /// Session publication.
    persistence_warning: Option<auth::owner::PersistenceWarning>,
    /// The insecure-only verdict the failure read-out is about (`SessionSnapshot::plaintext`),
    /// synced every `resync`.
    plaintext: Option<auth::PlaintextVerdict>,
    /// The shared question, asked on the report's alert ([`Sheet::Plaintext`]).
    question: PlaintextQuestion,
    ground: RouteGround,
    report: Report,
    state: LoginState,
}

impl LoginScreen {
    pub(crate) fn new(entry: EntryId, auth: auth::SessionRead<'_>) -> Self {
        let mut s = Self {
            entry,
            spin_ms: 0.0,
            phase_ms: 0.0,
            spin_phase: crate::ui::motion::Phase::default(),
            phase_clock: crate::ui::motion::Phase::default(),
            wait: (Phase::Idle, 0),
            qr_tex: 0,
            qr_tex_gen: 0,
            qr_px: (0, 0),
            qr_png_pending: None,
            phase: Phase::Idle,
            qr_gen: 0,
            qr_code: Arc::from(""),
            qr_replaced: false,
            error: Arc::from(""),
            delete_leftovers: 0,
            next_correlation: Some(1),
            pending_restart: None,
            persistence_warning: None,
            plaintext: None,
            question: PlaintextQuestion::new(),
            ground: RouteGround::new(),
            report: Report::new(),
            state: LoginState {
                phase: 0,
                qr_gen: 0,
                qr_replaced: false,
                has_control: false,
                delete_leftovers: 0,
                next_correlation: Some(1),
                pending_restart: None,
                warning: false,
                report: ReportState::default(),
                plaintext: None,
            },
        };
        // Read once at construction — not a `draw`-time poll — so the first frame is coherent
        // even when Mount/Tick are still queued behind it.
        // Without it, a mount that lands straight in `Phase::Deleted` (Settings' "Delete all local
        // data", confirmed) would draw its FIRST frame from the constructor's own placeholder
        // `Phase::Idle` — the wrong branch — because the container's default `Enter` (and this
        // screen's own first `draw`) both run before this screen's first `Tick` ever could.
        s.wait = s.resync(auth);
        s
    }

    /// Refresh every field this screen caches from one immutable Session publication. Called once
    /// at construction and again on every `Tick` through [`AuthLike`].
    ///
    /// **Returns the `(phase, qr_generation)` pair it just sampled**, so a caller that ALSO needs
    /// to know what changed — `tick`'s own wait-restart clock — reads the exact values this method
    /// cached instead of asking for a second publication. `tick` used to do exactly that: it
    /// computed `let live: Wait = (auth::phase(), auth::qr_generation());` several lines before
    /// calling `resync`, which then read the identical pair out of `crate::auth` again on its own.
    /// That is precisely the "read it twice" bug the module doc calls out by name — a retry (or a
    /// pin running out and being replaced) landing in the gap between the two independent reads
    /// could hand the wait-restart clock a phase that disagreed with the one this method cached,
    /// so the clock could reset (or fail to reset) against a `Wait` that was never actually drawn.
    /// Threading the sample through the return value makes the two agree by construction.
    fn resync(&mut self, auth: auth::SessionRead<'_>) -> Wait {
        let snapshot = auth.0;
        self.phase = snapshot.phase;
        self.qr_gen = snapshot.qr_generation;
        if self.phase == Phase::Waiting {
            // ONE read of the code, generation and bitmap together — `auth::QrCode`'s own doc says
            // why: taking them separately can mix the new digits with the old bitmap, or cache a
            // fresh bitmap under a stale generation.
            self.qr_code = Arc::clone(&snapshot.code);
            self.qr_replaced = snapshot.code_replaced;
            self.qr_png_pending = Some((snapshot.qr_generation, Arc::clone(&snapshot.png)));
        }
        if self.phase == Phase::Error {
            self.error = Arc::clone(&snapshot.error);
        }
        if self.phase == Phase::Deleted {
            self.delete_leftovers = snapshot.delete_leftovers;
        }
        self.persistence_warning = snapshot.persistence_warning;
        self.report.offer = snapshot.incident.clone();
        self.report.link_trouble = snapshot.link_trouble;
        self.plaintext = snapshot.plaintext.clone();
        self.sync_state();
        (self.phase, self.qr_gen)
    }

    fn sync_state(&mut self) {
        self.state = LoginState {
            phase: phase_disc(self.phase),
            qr_gen: self.qr_gen,
            qr_replaced: self.qr_replaced,
            has_control: self.has_control(),
            delete_leftovers: self.delete_leftovers as u32,
            next_correlation: self.next_correlation,
            pending_restart: self.pending_restart.map(|pending| pending.correlation),
            warning: self.persistence_warning.is_some(),
            report: self.report.state(),
            plaintext: self.plaintext_asks().then(|| {
                self.report.alert.is_open() && self.report.sheet == Sheet::Plaintext
            }),
        };
    }

    fn tick<H: AuthLike>(&mut self, t: Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let had_control = self.has_control();
        let alert_was_open = self.report.alert.is_open();

        // ONE sample of `crate::auth` feeds both the wait-restart clock below and every cached
        // field `resync` publishes — see `resync`'s own doc, and the module doc's "read it twice
        // is an observable bug" rule. This used to read `(auth::phase(), auth::qr_generation())`
        // again independently right here, a few lines before calling `resync`, which read the
        // identical pair a second time on its own; a retry (or an automatic pin replacement)
        // landing in the gap between the two reads could disagree with itself within one tick.
        let live: Wait = self.resync(H::auth(cx));

        // Each wait gets its own clock. A flow that walks Creating → Waiting → Discovering is
        // making progress, and restarting the timer at every step is what stops a slow-but-healthy
        // sign-in from being offered a way out of itself.
        if wait_restarted(self.wait, live) {
            self.wait = live;
            self.pending_restart = None;
            self.phase_clock.reset(t);
            self.phase_ms = 0.0;
        }

        // Both clocks are `motion::Phase` (spec phase 12 D4): the raw `+= dt` this used to be, and
        // the `fx.note(Motion)` below it, are now one call each — `advance` both reads the elapsed
        // ms AND reports motion, so a caller that forgets to note motion for a still-running clock
        // (the exact "ships frozen" bug class `control_has_spinner`'s own doc names) cannot
        // separate the two any more. Both clocks are gated on the SAME `control_has_spinner`
        // condition as the `fx.note` call they replace: `working_phase`/`Phase::Waiting`, the only
        // phases whose escape thresholds `phase_ms` is timed against, are themselves a SUBSET of
        // `control_has_spinner`'s true set, so this changes nothing about when the escape offer
        // can appear — it only stops accumulating (freezes, harmlessly, since nothing reads it)
        // while the spinner is not drawn at all (`Error`/`Deleted`).
        //
        // A report on its way draws the same spinner beside its line, on a read-out that draws
        // none of its own (a failed sign-in), so the spinner's clock also runs while it is busy —
        // and ONLY the spinner's: `phase_ms` stays on the control's own condition.
        let control_spins = control_has_spinner(self.phase, self.persistence_warning.is_some());
        let report_spins = self.report_note().is_some_and(|n| n.busy);
        if control_spins || report_spins {
            let mut present = fx.present();
            self.spin_ms = self.spin_phase.advance(t, &mut present);
            if control_spins {
                self.phase_ms = self.phase_clock.advance(t, &mut present);
            }
        }

        self.tick_report(t, cx, fx);

        let reseat = (self.has_control() && !had_control)
            || (alert_was_open && !self.report.alert.is_open() && self.has_control());
        if reseat && !self.report.alert.is_open() {
            // The control just appeared (a stalled wait grew its escape, or a phase moved straight
            // to `Error`) — seat focus on it now. The container's default `Enter` already ran, at
            // mount time, against whatever `groups()` answered THEN; nothing else will ever ask
            // the engine to look again unless this screen does, exactly the correction
            // `screens::onboard`'s first-run constructor makes for the same underlying reason
            // (that module's own doc has the longer argument for why a REACTION is the right shape
            // rather than a second write at some earlier point).
            Self::enter_group(fx, CONTROL_GROUP);
        }
        self.sync_state();
    }

    fn enter_elem<H: AppLike>(fx: &mut Effects<'_, H>, key: crate::ui::machine::FocusKey<u32>) {
        let me = fx.from();
        fx.push(Fx::Deliver(
            me,
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(key) })),
        ));
    }

    fn enter_group<H: AppLike>(fx: &mut Effects<'_, H>, group: GroupId) {
        let me = fx.from();
        fx.push(Fx::Deliver(
            me,
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::ContainerGroup(group),
            })),
        ));
    }

    /// The onboarding report's frame: resolve a pending offer, put an offered one on screen, take
    /// the alert down when the offer it asks about has gone, and step the motion.
    fn tick_report<H: AuthLike>(&mut self, t: Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        use auth::owner::IncidentState as S;
        let offer = self.report.offer.clone();
        let id = offer.as_ref().map(|o| o.id);
        if self.report.support_for != id {
            // A different failure: its own support line, and a Details card still up for the
            // old one has nothing left to describe.
            self.report.support_for = id;
            if self.report.sheet == Sheet::Details && self.report.alert.is_open() {
                self.report.alert.close();
            }
            self.report.support = offer
                .as_ref()
                .and_then(|o| CString::new(support_line(o)).ok())
                .unwrap_or_default();
        }
        if let Some(o) = &offer {
            // **The screen that shows the failure resolves it**, at the permission it reads now:
            // the decision can change while the failure is on screen (a withdrawal elsewhere),
            // and an offer made under an older decision is resolved again. A harness-driven boot
            // never stops on the question, so it never asks for one.
            let revision = crate::telemetry::consent::revision();
            let stale = match o.state {
                S::Pending => true,
                S::Offered { revision: at } | S::OnRequest { revision: at } => at != revision,
                _ => false,
            };
            if stale
                && !crate::dev::scenarios::harness_driven()
                && self.report.last_resolve != Some((o.id, revision))
            {
                self.report.last_resolve = Some((o.id, revision));
                let permission = crate::telemetry::consent::report_permission_now(
                    crate::telemetry::consent::ONBOARDING_REPORT_SCOPE,
                );
                fx.push(Fx::App(AppFx::Session(auth::SessionCmd::ResolveIncident {
                    id: o.id,
                    permission,
                    revision,
                })));
            }
            // An eligible plaintext-only server is a CHOICE, not a failure: the read-out asks
            // it through *Connect*, and the report stays one press away behind *Details*.
            if matches!(o.state, S::Offered { .. })
                && !self.plaintext_eligible()
                && !self.report.alert.visible()
                && self.report.alert_for != Some(o.id)
            {
                self.report.alert_for = Some(o.id);
                self.report.sheet = Sheet::Question;
                self.report.alert.open_with_body(REPORT_QUESTION, REPORT_BODY);
                Self::enter_group(fx, ALERT_GROUP);
            }
        }
        let still_asked = offer.as_ref().is_some_and(|o| {
            matches!(o.state, S::Offered { .. }) && self.report.alert_for == Some(o.id)
        });
        if self.report.alert.is_open() && self.report.sheet == Sheet::Question && !still_asked {
            // The question is no longer the one on the table — answered elsewhere, superseded,
            // or erased by a sign-out. Nothing to answer, so no fade either.
            self.report.alert.close();
        }
        if self.report.alert.is_open() && self.report.sheet == Sheet::Plaintext && !self.plaintext_asks() {
            // The server the question is about is no longer the failure on screen.
            self.question.withdraw(&mut self.report.alert);
        }
        if self.report.alert.is_open() && self.report.sheet == Sheet::Details {
            use crate::ui::decision_alert::Answers;
            let body = self.report.details_body();
            let answers = if self.report.sendable() { Answers::Two } else { Answers::One };
            self.report.alert.reconcile_card(DETAILS_TITLE, body, answers);
        }
        self.report.alert.update(t.dt());
        let focused = cx
            .focus
            .current
            .filter(|k| k.entry == self.entry)
            .and_then(|k| slot_of(k.elem));
        let pops = if self.report.alert.visible() { None } else { focused };
        self.report.pop.step(pops, t.dt());
    }

    fn control_kind(&self) -> Option<ControlKind> {
        if self.persistence_warning.is_some() {
            return Some(ControlKind::ContinueUnsaved);
        }
        match self.phase {
            Phase::Deleted => Some(ControlKind::StartLogin),
            Phase::Error if self.plaintext_asks() => Some(ControlKind::ConnectPlaintext),
            Phase::Error => Some(ControlKind::Retry),
            Phase::Waiting if qr_escape_offered(self.phase_ms) => Some(ControlKind::RestartWait),
            p if working_phase(p) && escape_offered(self.phase_ms) => {
                Some(ControlKind::RestartWait)
            }
            _ => None,
        }
    }

    /// Whether the failure on screen is an eligible plaintext-only server — answered or not. Such
    /// a failure is a choice, not a fault, so the report question is not raised over it.
    fn plaintext_eligible(&self) -> bool {
        self.phase == Phase::Error && self.plaintext.as_ref().is_some_and(|v| v.offers())
    }

    /// Whether the failure on screen still ASKS: eligible and not yet answered
    /// (`plaintext_question::asks`).
    fn plaintext_asks(&self) -> bool {
        self.phase == Phase::Error && plaintext_question::asks(self.plaintext.as_ref())
    }

    /// Ask "Connect without encryption?" — the shared question on this screen's alert.
    fn open_plaintext<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        let Some(v) = self.plaintext.as_ref().filter(|_| self.plaintext_asks()) else {
            return;
        };
        self.report.sheet = Sheet::Plaintext;
        self.question.open(&mut self.report.alert, &v.machine_id, None);
        Self::enter_group(fx, ALERT_GROUP);
    }

    /// Whether ANY element of the control group is on screen — the primary, or the report's
    /// *Details*.
    fn has_control(&self) -> bool {
        self.row().n > 0
    }

    /// Whether the held incident offers its *Details*: on the failure read-out, and on the QR
    /// screen while the wait itself is what failed, or for the helper failure on a save warning.
    fn details_offered(&self) -> bool {
        if let Some(warning) = self.persistence_warning {
            return warning.helper.is_some() && self.report.offer.as_ref().is_some_and(|o|
                o.key.kind == crate::telemetry::incident::IncidentKind::SaveFailed);
        }
        match (&self.report.offer, self.phase) {
            (Some(_), Phase::Error) => true,
            (Some(o), Phase::Waiting) => {
                o.key.kind == crate::telemetry::incident::IncidentKind::LinkStalled
            }
            _ => false,
        }
    }

    /// The control group in walk order. On the read-out the primary leads (it is the row's centre
    /// of gravity); on the QR screen *Details* sits bottom-left in the narrative column and the
    /// escape sentence in the content column, so the walk goes left to right.
    fn row(&self) -> Row {
        let mut row = Row::default();
        let primary = self.control_kind().is_some();
        let report = |row: &mut Row| {
            if self.details_offered() {
                row.push(DETAILS);
            }
        };
        if self.phase == Phase::Waiting && self.persistence_warning.is_none() {
            report(&mut row);
            if primary {
                row.push(CONTROL);
            }
        } else {
            if primary {
                row.push(CONTROL);
            }
            report(&mut row);
        }
        row
    }

    /// **The report's one status line** — shared by the failed read-out and the QR screen, so the
    /// two say the same thing: `None` until somebody has acted or a standing Yes sent one. A report
    /// still on its way is `busy` on both screens, which each draw the shared inline spinner
    /// beside it. The Report ID and the support line are never here — they are the Details card's.
    fn report_note(&self) -> Option<Note> {
        if let Some(helper) = self.persistence_warning.and_then(|warning| warning.helper) {
            return Some(Note { text: CString::new(helper.line()).unwrap(), busy: false });
        }
        if !self.details_offered() {
            return None;
        }
        let (text, busy) = self.report.status()?;
        Some(Note { text: text.to_owned(), busy })
    }

    /// Open the Details card for the held report: the Report ID and the support line, *Close*,
    /// and *Send report* only while Session would accept it — which then holds focus.
    fn open_details<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        if self.report.offer.is_none() {
            return;
        }
        use crate::ui::decision_alert::Answers;
        let sendable = self.report.sendable();
        let body = self.report.details_body();
        self.report.sheet = Sheet::Details;
        self.report
            .alert
            .open_card(DETAILS_TITLE, body, if sendable { Answers::Two } else { Answers::One });
        if sendable {
            self.report.alert.set_choice(Choice::Destructive);
        }
        Self::enter_group(fx, ALERT_GROUP);
    }

    /// The QR screen's *Details* pill, at the head of the route's bottom action band.
    fn waiting_details(&self, measure: &dyn Measure) -> Option<Rect> {
        if !self.details_offered() {
            return None;
        }
        let w = Button::pill_w_measured(DETAILS_LABEL, theme::size::BODY, false, false, measure);
        Some(RouteLayout::screen().action_pair(w, 0.0).0)
    }

    /// The read-out's control labels by slot (primary, *Details*), and whether the read-out
    /// carries a reason. How tall the reason is — one line, or a `Failed` read-out's
    /// two-line slot — is the widget's answer from the kind.
    fn readout_labels(&self) -> ([Option<&'static CStr>; 2], bool) {
        let Some(kind) = self.control_kind() else {
            return ([None; 2], false);
        };
        let has_reason = match kind {
            ControlKind::RestartWait => true, // Working's own stall reason is unconditional once offered
            ControlKind::Retry => !self.error.is_empty(),
            ControlKind::StartLogin => true, // `deleted_readout` always states one
            ControlKind::ContinueUnsaved => true, // the warning sentence is unconditional
            ControlKind::ConnectPlaintext => true, // `auth::insecure_only_copy` always states one
        };
        let details = self.details_offered().then_some(DETAILS_LABEL);
        ([Some(label_for(kind)), details], has_reason)
    }

    /// The read-out's treatment — the one answer `draw` and the control geometry both read: the
    /// unconfirmed save and a failed sign-in are `Failed`, the finished delete `Empty`, a phase
    /// still in flight `Working`.
    fn readout_kind(&self) -> StatusKind {
        if self.persistence_warning.is_some() {
            return StatusKind::Failed;
        }
        match self.phase {
            Phase::Error => StatusKind::Failed,
            Phase::Deleted => StatusKind::Empty,
            _ => StatusKind::Working,
        }
    }

    /// Every element's rect — shared verbatim by `draw`'s stops and every `Focusable` query, so
    /// the two can never drift apart (`ui/table_screen.rs`'s rule).
    fn elem_rect(&self, elem: u32, measure: &dyn Measure) -> Option<Rect> {
        self.row().position(elem)?;
        if self.phase == Phase::Waiting && self.persistence_warning.is_none() {
            return match elem {
                // The QR screen's escape is a SENTENCE, not a button (see `waiting_status`'s doc),
                // so its geometry is the status line's own rect rather than a computed pill.
                CONTROL => Some(qr_layout(RouteLayout::screen()).status),
                DETAILS => self.waiting_details(measure),
                _ => None,
            };
        }
        let (labels, has_reason) = self.readout_labels();
        let frames = status_row_rects(measure, labels, self.readout_kind(), has_reason);
        frames[slot_of(elem)?]
    }

    /// The control, pressed. Every action is a typed Session command. Restart additionally records
    /// its addressed correlation and leaves the elapsed clock untouched until acceptance returns.
    fn allocate_reply<H: AppLike>(&mut self, fx: &Effects<'_, H>) -> Option<auth::owner::ReplyTo> {
        let crate::ui::machine::MachineId::Instance(instance) = fx.from() else {
            return None;
        };
        let correlation = self.next_correlation?;
        let next = correlation.checked_add(1)?;
        self.next_correlation = Some(next);
        Some(auth::owner::ReplyTo {
            instance: instance.0,
            correlation,
        })
    }

    fn request_root_back<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        let Some(reply) = self.allocate_reply(fx) else {
            self.sync_state();
            return;
        };
        fx.push(Fx::App(AppFx::Session(auth::SessionCmd::BackAtRoot {
            reply,
        })));
        self.sync_state();
    }

    /// An element of the control group, pressed.
    fn activate_elem<H: AppLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        if self.row().position(elem).is_none() {
            // Not on screen this frame — a control that is not drawn is never activated.
            return;
        }
        match elem {
            DETAILS => {
                self.open_details(fx);
                self.sync_state();
            }
            _ => self.activate(fx),
        }
    }

    /// The alert's answer. On the question, Not now and BACK decline for this launch and Send
    /// report is the whole of the one-off's consent; either way focus returns to the read-out,
    /// where *Details* keeps the report one press away. On the Details card, Close and BACK only
    /// close it and Send report sends it (Session re-checks that it still can); either way focus
    /// returns to the *Details* that opened it.
    fn alert_answer<H: AppLike>(&mut self, send: bool, fx: &mut Effects<'_, H>) {
        if !self.report.alert.is_open() {
            return;
        }
        if self.report.sheet == Sheet::Plaintext {
            // *Connect* allows this server and retries at once; *Not now* and BACK record the
            // refusal, and the read-out says how to be asked again. Session persists either. The
            // shared question dismisses the alert.
            if let Some(cmd) = self.question.answer(&mut self.report.alert, send) {
                fx.push(Fx::App(AppFx::Session(cmd)));
            }
            if self.has_control() {
                Self::enter_group(fx, CONTROL_GROUP);
            }
            self.sync_state();
            return;
        }
        self.report.alert.dismiss();
        if self.report.sheet == Sheet::Details {
            if let Some(o) = self.report.offer.as_ref().filter(|o| send && o.sendable()) {
                crate::log("login: user sent a sign-in report from Details");
                fx.push(Fx::App(AppFx::Session(auth::SessionCmd::ReportIncident { id: o.id })));
            }
            if self.row().position(DETAILS).is_some() {
                Self::enter_elem(fx, self.key(DETAILS));
            } else if self.has_control() {
                Self::enter_group(fx, CONTROL_GROUP);
            }
            self.sync_state();
            return;
        }
        if let Some(o) = &self.report.offer {
            let id = o.id;
            fx.push(Fx::App(AppFx::Session(if send {
                auth::SessionCmd::ReportIncident { id }
            } else {
                auth::SessionCmd::DeclineIncident { id }
            })));
        }
        if self.has_control() {
            Self::enter_group(fx, CONTROL_GROUP);
        }
        self.sync_state();
    }

    fn activate<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        match self.control_kind() {
            Some(ControlKind::ContinueUnsaved) => {
                if let Some(warning) = self.persistence_warning {
                    fx.push(Fx::App(AppFx::Session(
                        auth::SessionCmd::AcknowledgePersistenceWarning { key: warning.key },
                    )));
                }
            }
            Some(ControlKind::StartLogin) => {
                fx.push(Fx::App(AppFx::Session(auth::SessionCmd::StartLogin)));
            }
            Some(ControlKind::Retry) => {
                fx.push(Fx::App(AppFx::Session(auth::SessionCmd::Retry)));
            }
            Some(ControlKind::ConnectPlaintext) => self.open_plaintext(fx),
            Some(ControlKind::RestartWait) => {
                // "requested", not "restarted": the press may still be refused (the flow moved on
                // between this screen's last `Tick` and this key), and the event log is the one
                // place that failure is read from — a claim it did something is exactly the wrong
                // thing to have written there.
                crate::log("login: user requested a restart of a stalled sign-in");
                if self.pending_restart.is_none() {
                    if let Some(reply) = self.allocate_reply(fx) {
                        self.pending_restart = Some(PendingRestart {
                            correlation: reply.correlation,
                            wait: self.wait,
                        });
                        fx.push(Fx::App(AppFx::Session(auth::SessionCmd::RestartWait {
                            phase: self.wait.0,
                            qr_generation: self.wait.1,
                            reply,
                        })));
                    }
                }
            }
            None => {}
        }
        self.sync_state();
    }

    fn restart_reply(&mut self, request: u32, correlation: u32, accepted: bool) {
        if request != correlation {
            return;
        }
        let Some(pending) = self.pending_restart else {
            return;
        };
        if pending.correlation != correlation || pending.wait != self.wait {
            return;
        }
        self.pending_restart = None;
        if accepted {
            self.phase_ms = 0.0;
            self.phase_clock = crate::ui::motion::Phase::default();
        }
        self.sync_state();
    }

    fn draw_readout<H: AppLike>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        p: Painter,
        env: &Env,
        caption: &CStr,
        kind: StatusKind,
        reason: Option<&CStr>,
        focus: Option<u32>,
    ) {
        let note = self.report_note();
        let o = self.readout(caption, kind, reason, note.as_ref());
        self.draw_overlay(f, p, env, o, focus);
    }

    /// **The read-out as it is drawn** — the one overlay `draw_overlay` paints and the tests read.
    /// Its controls are [`Self::readout_labels`], the same answer the geometry and the press read,
    /// so the pill on screen always names what OK does: no stage passes a label of its own.
    fn readout<'a>(
        &self,
        caption: &'a CStr,
        kind: StatusKind,
        reason: Option<&'a CStr>,
        note: Option<&'a Note>,
    ) -> StatusOverlay<'a> {
        debug_assert_eq!(kind, self.readout_kind(), "the geometry reads the same kind the draw paints");
        let (labels, _) = self.readout_labels();
        readout_overlay(caption, kind, reason, labels, note).phase(self.spin_ms as u32)
    }

    /// The failed sign-in's read-out: the verdict, the phase's reason and its controls.
    fn failed_readout<'a>(&self, reason: &'a CStr, note: Option<&'a Note>) -> StatusOverlay<'a> {
        self.readout(
            c"Couldn\u{2019}t sign in",
            StatusKind::Failed,
            (!reason.is_empty()).then_some(reason),
            note,
        )
    }

    fn draw_overlay<H: AppLike>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        p: Painter,
        env: &Env,
        mut o: StatusOverlay<'_>,
        focus: Option<u32>,
    ) {
        let press = f.press.scale;
        let pop = &self.report.pop;
        if o.action.is_some() {
            o = o
                .focus(focus.and_then(slot_of))
                .scales([0, 1].map(|i| pop.scale_with(i, press)));
        }
        o.draw_measured(env, p, f.measure);
        if self.report.alert.visible() {
            // The alert owns the pointer while it is up; nothing under it is a target.
            return;
        }
        let frames = o.action_frames_measured(f.measure);
        for elem in self.row().as_slice() {
            let Some(rect) = slot_of(*elem).and_then(|i| frames[i]) else {
                continue;
            };
            self.control_stop(f, p, *elem, rect);
        }
    }

    fn control_stop<H: AppLike>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, elem: u32, rect: Rect) {
        f.stop(
            p,
            Stop {
                key: crate::ui::machine::FocusKey {
                    entry: self.entry,
                    elem,
                },
                rect,
                rest_rect: rect,
                clip: Rect::FULL,
                hover: Hover::Focus,
                activate: Activate::Direct,
            },
        );
    }

    fn draw_working<H: AppLike>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        p: Painter,
        env: &Env,
        msg: &str,
        focus: Option<u32>,
    ) {
        let caption = CString::new(msg).unwrap_or_default();
        let stuck = self.has_control();
        self.draw_readout(
            f,
            p,
            env,
            &caption,
            StatusKind::Working,
            // The reason arrives WITH the control, and only then: it exists to explain why a
            // button just appeared under a spinner that was doing fine a moment ago.
            stuck.then_some(c"This is taking longer than usual."),
            focus,
        );
    }

    /// AUTH-03: the fresh save the disk could not confirm durable. Drawn instead of the phase's
    /// ordinary read-out whenever a warning is showing — see `draw`'s own check.
    fn draw_warning<H: AppLike>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        p: Painter,
        env: &Env,
        focus: Option<u32>,
    ) {
        self.draw_readout(
            f,
            p,
            env,
            c"Couldn\u{2019}t save your sign-in",
            StatusKind::Failed,
            Some(c"Your sign-in couldn\u{2019}t be saved on this TV. You can continue, but you\u{2019}ll be asked to sign in again next time."),
            focus,
        );
    }

    fn draw_failed<H: AppLike>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        p: Painter,
        env: &Env,
        focus: Option<u32>,
    ) {
        let reason = CString::new(self.error.as_ref()).unwrap_or_default();
        let note = self.report_note();
        let o = self.failed_readout(&reason, note.as_ref());
        self.draw_overlay(f, p, env, o, focus);
    }

    /// **Empty, not Failed.** Deleting everything is a completed action the user asked for, so it
    /// must not read as a failure — the same distinction `StatusKind::Empty` carries for a
    /// library with nothing in it. A partial one is still not a FAILURE either: what it did do, it
    /// did.
    fn draw_deleted<H: AppLike>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        p: Painter,
        env: &Env,
        focus: Option<u32>,
    ) {
        let (verdict, reason) = deleted_readout(self.delete_leftovers);
        self.draw_readout(
            f,
            p,
            env,
            verdict,
            StatusKind::Empty,
            Some(reason),
            focus,
        );
    }

    /// **The escape SENTENCE takes no focus look, unlike its three siblings' pills.**
    /// `draw_readout` (shared by `draw_working`/`draw_failed`/`draw_deleted`) paints its action as
    /// a real `Button` face, whose fill genuinely differs when focused — legacy hardcoded
    /// `.focused(true)` there because "the only control on the screen … holds focus by
    /// construction"; the owned screen instead threads the engine's actual focus state through,
    /// which happens to answer the same `true` here since there is still nowhere else focus could
    /// be. The QR screen's 60-second escape is not that kind of control: it is a SENTENCE (the
    /// module doc's own word for it), drawn as plain text beside a spinner with no button chrome
    /// for a `focused` bool to vary — legacy's own `draw_waiting` (what this ports) never took one
    /// either, for the identical reason, so this is not a dropped behaviour. Giving this sentence a
    /// focus-dependent look would be a NEW affordance, and a visual one belongs in front of
    /// `ui/CLAUDE.md`'s own design review, not slipped in unreviewed by a bug-fix pass. The report's
    /// *Details* pill is an ordinary control in the route's action band and DOES take `focus`.
    fn draw_waiting<H: AppLike>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, focus: Option<u32>) {
        let layout = RouteLayout::screen();
        layout.draw_narrative(
            p,
            None,
            "Sign in to Plex",
            "Use your phone camera to scan the code, or link this television manually with the address and code shown here.",
            theme::size::LABEL,
            f.measure,
        );
        let right = qr_layout(layout);

        TextView::new("plex.tv/link", theme::size::TITLE, theme::TEXT_HEADING)
            .bold()
            .h(HAlign::Center)
            .draw(p, right.url);

        // QR on a bright card (the white border is the scan quiet-zone). Plex's own PNG → we just
        // show it.
        let card = right.card;
        p.rrect(card, 24.0, 24.0, theme::SURFACE_QR_PLATE);
        if self.qr_tex != 0 {
            let pad = 30.0;
            let inner = Rect::new(
                card.x + pad,
                card.y + pad,
                card.w - 2.0 * pad,
                card.h - 2.0 * pad,
            );
            // Plex's PNG is WHITE modules on a transparent ground; tint black so the modules render
            // dark on the white card (the transparent ground shows the card) → a scannable
            // black-on-white QR.
            p.tex(self.qr_tex, inner, 0.0, theme::scrim_black(1.0));
        } else {
            Spinner::new(card.x + card.w * 0.5, card.y + card.h * 0.5, 22.0)
                .phase(self.spin_ms as u32)
                .tint(theme::scrim_black(0.5))
                .draw(&Env::inert(), p);
        }

        // The manual code and waiting state remain in the same right-column stack as the URL and
        // QR. Both use couch-readable type rungs; this is an alternative sign-in path, not fine
        // print.
        if let Ok(code) = CString::new(self.qr_code.to_uppercase()) {
            p.text(
                code.as_ptr(),
                right.code.cx(),
                right.code.y,
                theme::size::DISPLAY,
                theme::TEXT_PRIMARY,
                1,
                1,
            );
        }

        let wr = 15.0;
        let wy = right.status.cy();
        let escaping = qr_escape_offered(self.phase_ms);
        let status = waiting_status(self.qr_replaced, escaping, self.report.link_trouble);
        let status_w = f.measure.width(status, theme::size::BODY, false);
        let sx = right.status.cx() - (wr * 2.0 + theme::space::SM + status_w) * 0.5;
        Spinner::new(sx + wr, wy, wr)
            .phase(self.spin_ms as u32)
            .tint(theme::TEXT_SECONDARY)
            .draw(&Env::inert(), p);
        let ty = crate::text::text_vcenter_y(theme::size::BODY, 0, wy);
        p.text(
            status.as_ptr(),
            sx + wr * 2.0 + theme::space::SM,
            ty,
            theme::size::BODY,
            theme::TEXT_SECONDARY,
            0,
            0,
        );

        let details = self.waiting_details(f.measure);
        if let Some(rect) = details {
            Button::new(DETAILS_LABEL.as_ptr(), theme::size::BODY, rect)
                .focused(focus == Some(DETAILS))
                .scale(self.report.pop.scale_with(1, f.press.scale))
                .draw(&Env::inert(), p);
        }
        self.draw_disclosure(p, layout, f.measure);

        if self.report.alert.visible() {
            return;
        }
        if escaping {
            self.control_stop(f, p, CONTROL, right.status);
        }
        if let Some(rect) = details {
            self.control_stop(f, p, DETAILS, rect);
        }
    }

    /// The QR screen's report status — [`Self::report_note`], the failed read-out's own line — as
    /// fine print sitting on the action band, in the narrative column *Details* stands in.
    fn draw_disclosure(&self, p: Painter, layout: RouteLayout, measure: &dyn crate::ui::machine::Measure) {
        let bottom = layout.action.y - theme::space::MD;
        if let Some(note) = self.report_note() {
            // A report on its way: the shared inline spinner in a leading gutter, on the first
            // line's cap band (TextView sets each line cap-top), the text one gutter over.
            let gutter = if note.busy { Spinner::inline_gutter() } else { 0.0 };
            let col = Rect::new(layout.narrative.x + gutter, 0.0, layout.narrative.w - gutter, 0.0);
            let line = note.text.to_string_lossy();
            let view = TextView::new(&line, theme::size::CAPTION, theme::TEXT_TERTIARY).max_lines(2);
            let h = view.measure_h(col.w);
            let top = bottom - h;
            if note.busy {
                let cap = measure.cap_h(theme::size::CAPTION);
                Spinner::leading(layout.narrative.x, top + cap / 2.0)
                    .phase(self.spin_ms as u32)
                    .tint(theme::TEXT_TERTIARY)
                    .draw(&Env::inert(), p);
            }
            view.draw(p, Rect::new(col.x, top, col.w, h));
        }
    }

    /// The alert — the report question or the Details card — over whatever the phase drew, and its
    /// answers' stops once it has settled (a pointer hit is positional — `DecisionAlert::settled`).
    fn draw_alert<H: AppLike>(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        if !self.report.alert.visible() {
            return;
        }
        self.report.alert.draw_scrim();
        let (cancel, affirm) = match self.report.sheet {
            Sheet::Question => (NOT_NOW, SEND_REPORT),
            Sheet::Details => (CLOSE, SEND_REPORT),
            Sheet::Plaintext => PlaintextQuestion::verbs(),
        };
        self.report.alert.draw(cancel, affirm);
        let frames = self.report.alert.frames();
        self.report.alert_frames = Some(frames);
        if self.report.alert.is_open() && self.report.alert.settled() {
            for &elem in self.alert_elems() {
                let rect = if elem == ALERT_SEND { frames.1 } else { frames.0 };
                f.stop(
                    Painter::root(),
                    Stop {
                        key: crate::ui::machine::FocusKey { entry: self.entry, elem },
                        rect,
                        rest_rect: rect,
                        clip: Rect::FULL,
                        hover: Hover::Focus,
                        activate: Activate::Press,
                    },
                );
            }
        }
    }

    /// Build+free the QR texture. GL only, called from [`Screen::prepare`] — never from `step`, so
    /// a host test driving `Machine::step` alone never touches it.
    fn prepare_qr_tex(&mut self) {
        self.drop_stale_qr_tex(self.qr_gen);
        let Some((gen, png)) = self.qr_png_pending.take() else {
            return;
        };
        // The belt to the drop above's braces: a code replaced again between the `Tick` that
        // captured this PNG and this `prepare` (rare — it needs two replacements inside one frame)
        // must not upload a bitmap for a code that has already died.
        self.drop_stale_qr_tex(gen);
        if self.qr_tex != 0 || png.is_empty() {
            return;
        }
        let (mut w, mut h): (c_int, c_int) = (0, 0);
        let px = crate::img::img_decode_rgba(png.as_ptr(), png.len() as c_int, &mut w, &mut h);
        if !px.is_null() {
            self.qr_tex = crate::img::img_upload_rgba(px, w, h);
            self.qr_px = if self.qr_tex != 0 {
                (w.max(0) as u32, h.max(0) as u32)
            } else {
                (0, 0)
            };
            self.qr_tex_gen = gen;
            crate::img::img_free(px);
        }
    }

    fn drop_stale_qr_tex(&mut self, live: u64) {
        if !qr_cache_stale(self.qr_tex_gen, live, self.phase) {
            return;
        }
        crate::gfx::delete_tex(self.qr_tex);
        self.qr_tex = 0;
        self.qr_px = (0, 0);
        self.qr_tex_gen = live;
    }
}

impl LoginScreen {
    /// The alert's answer rects as last drawn. Before the first draw there is no measured panel;
    /// the answers are not stops until the sheet has settled anyway.
    fn alert_rect(&self, elem: u32) -> Rect {
        self.report
            .alert_frames
            .map(|(not_now, send)| if elem == ALERT_SEND { send } else { not_now })
            .unwrap_or(Rect::FULL)
    }

    fn key(&self, elem: u32) -> crate::ui::machine::FocusKey<u32> {
        crate::ui::machine::FocusKey { entry: self.entry, elem }
    }

    /// The open alert's answers: both, or — a Details card with nothing left to send — *Close*
    /// alone.
    fn alert_elems(&self) -> &'static [u32] {
        match self.report.alert.answers() {
            crate::ui::decision_alert::Answers::Two => &[ALERT_CANCEL, ALERT_SEND],
            crate::ui::decision_alert::Answers::One => &[ALERT_CANCEL],
        }
    }
}

/// **Two groups, never at once.** While the alert is OPEN its answers are the only group — the player's repair alert's shape — and the read-out's controls return the moment it
/// is answered (its fade is drawn, not focusable: `groups` gates on `is_open`, not `visible`, so
/// the focus a dismissal hands back has somewhere to land).
impl<H: AppLike> Focusable<H> for LoginScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if self.report.alert.is_open() {
            out.push(GroupSpec {
                id: ALERT_GROUP,
                kind: GroupKind::Row { wrap: false },
                seat: Seat::First,
                reachable: AxisMask::BOTH,
                edge: [EdgeRule::Stop; 4],
                extent: self
                    .alert_elems()
                    .iter()
                    .map(|&e| self.alert_rect(e))
                    .reduce(|a, b| a.union(b))
                    .unwrap_or(Rect::FULL),
                len: self.alert_elems().len(),
                elem: ElemKind::Control,
            });
            return;
        }
        let row = self.row();
        let mut extent: Option<Rect> = None;
        for elem in row.as_slice() {
            if let Some(r) = self.elem_rect(*elem, cx.measure) {
                extent = Some(extent.map_or(r, |e| e.union(r)));
            }
        }
        let Some(extent) = extent else {
            return;
        };
        out.push(GroupSpec {
            id: CONTROL_GROUP,
            kind: GroupKind::Free,
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            // Nothing else is ever focusable on this screen, so a walk off either end of the
            // group stays put rather than searching for a sibling group that does not exist.
            edge: [EdgeRule::Stop; 4],
            extent,
            len: row.n,
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        if self.report.alert.is_open() {
            return self.alert_elems().contains(key).then_some(ALERT_GROUP);
        }
        self.row().position(*key).map(|_| CONTROL_GROUP)
    }
    fn neighbour(
        &self,
        key: crate::ui::machine::FocusKey<u32>,
        dir: Dir,
        _cx: &Cx<'_, H>,
    ) -> Step<u32> {
        if self.report.alert.is_open() {
            return match (key.elem, dir) {
                (ALERT_CANCEL, Dir::Right) if self.alert_elems().contains(&ALERT_SEND) => {
                    Step::Move(self.key(ALERT_SEND))
                }
                (ALERT_SEND, Dir::Left) => Step::Move(self.key(ALERT_CANCEL)),
                _ => Step::Edge,
            };
        }
        // The group is one walk: LEFT/UP to the previous element, RIGHT/DOWN to the next.
        let row = self.row();
        let Some(at) = row.position(key.elem) else {
            return Step::Edge;
        };
        let next = match dir {
            Dir::Left | Dir::Up => at.checked_sub(1),
            Dir::Right | Dir::Down => Some(at + 1).filter(|&i| i < row.n),
        };
        next.map_or(Step::Edge, |i| Step::Move(self.key(row.elems[i])))
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        if self.report.alert.is_open() {
            if !self.alert_elems().contains(key) {
                return None;
            }
            let rect = self.alert_rect(*key);
            return Some(Placed {
                rect,
                rest_rect: rect,
                clip: Rect::FULL,
                index: Some((*key == ALERT_SEND) as u32),
            });
        }
        let index = self.row().position(*key)?;
        let rect = self.elem_rect(*key, cx.measure)?;
        Some(Placed {
            rect,
            rest_rect: rect,
            clip: Rect::FULL,
            index: Some(index as u32),
        })
    }
    /// A focused element that has left the row — an alert answer once the alert is gone, a primary
    /// that stopped being offered — hands focus to *Details* when it is still there, otherwise to
    /// the head of the group.
    fn reconcile(
        &self,
        want: crate::ui::machine::FocusKey<u32>,
        cx: &Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        if self.group_of(&want.elem, cx).is_some() {
            return want;
        }
        if self.report.alert.is_open() {
            // Content reconciliation can remove Send while Details stays open. The shared card
            // has already retained a valid choice; map that choice back to the engine's keys.
            return self.key(match self.report.alert.choice() {
                Choice::Cancel => ALERT_CANCEL,
                Choice::Destructive => ALERT_SEND,
            });
        }
        let row = self.row();
        if row.n == 0 {
            return want;
        }
        let elem = if row.position(DETAILS).is_some() { DETAILS } else { row.elems[0] };
        self.key(elem)
    }
    fn seat(
        &self,
        g: GroupId,
        _from: Placed,
        _cx: &Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        if g == ALERT_GROUP {
            // The Details card opens on *Send report* while there is one; the question on *Not now*.
            let send = self.report.sheet == Sheet::Details && self.alert_elems().contains(&ALERT_SEND);
            return self.key(if send { ALERT_SEND } else { ALERT_CANCEL });
        }
        let row = self.row();
        self.key(if row.n > 0 { row.elems[0] } else { CONTROL })
    }
}

impl<H: AuthLike> Machine<H> for LoginScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        // **The alert traps everything under it** while it is on screen, fade included — the
        // player repair alert's trap, verbatim in shape: its answers are `Control` elements (the
        // press dip, a commit on release), BACK is the cancel slot (*Not now* / *Close*), the
        // arrows and OK go on to the engine for the answers, and nothing reaches the read-out.
        if self.report.alert.visible() {
            match ev {
                ScreenEvent::FocusMoved { to, .. } => {
                    self.report.alert.set_choice(if to.elem == ALERT_SEND {
                        Choice::Destructive
                    } else {
                        Choice::Cancel
                    });
                    return Handled::Yes;
                }
                ScreenEvent::PressCommit(_) => {
                    if let Some(key) = cx.focus.current {
                        if self.alert_elems().contains(&key.elem) {
                            self.alert_answer(key.elem == ALERT_SEND, fx);
                        }
                    }
                    return Handled::Yes;
                }
                ScreenEvent::Activate(_) => return Handled::Yes,
                ScreenEvent::Input(input) => {
                    if !self.report.alert.is_open() {
                        return Handled::Yes;
                    }
                    use crate::ui::consts;
                    return match input.kind {
                        InputKind::Key { sym, wcode, edge, .. } => match consts::classify(sym, wcode) {
                            consts::Key::Back | consts::Key::Stop if edge == Edge::Down => {
                                self.alert_answer(false, fx);
                                Handled::Yes
                            }
                            consts::Key::Left { .. }
                            | consts::Key::Right { .. }
                            | consts::Key::Ok
                            | consts::Key::Exit => Handled::No,
                            _ => Handled::Yes,
                        },
                        InputKind::Pointer { .. } | InputKind::Click { .. } => Handled::No,
                        _ => Handled::Yes,
                    };
                }
                _ => {}
            }
        }
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(*t, cx, fx);
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => {
                self.activate_elem(*elem, fx);
                fx.invalidate(crate::ui::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Async(
                crate::ui::machine::RequestId(request),
                AppMsg::RestartReply {
                    correlation,
                    accepted,
                },
            ) => {
                self.restart_reply(*request, *correlation, *accepted);
                Handled::Yes
            }
            ScreenEvent::Async(
                crate::ui::machine::RequestId(request),
                AppMsg::BackReply { correlation, .. },
            ) if request == correlation => Handled::Yes,
            // BACK asks Session for its addressed root decision; Session/core owns stored-session
            // resume, cooldown and platform handling. (An open Details card is the alert, and the
            // trap above has already made BACK its *Close*.)
            ScreenEvent::Input(InputEvent {
                kind:
                    InputKind::Key {
                        key: Key::Back,
                        edge: Edge::Down,
                        ..
                    },
                ..
            }) => {
                self.request_root_back(fx);
                Handled::Yes
            }
            // **The QR texture has to be freed exactly here, and `Unmount` is the one event this
            // screen is guaranteed to see before it dies.** `AppMounter::mount` builds a brand new
            // `LoginScreen` on every `Replace` onto `Route::Login` (`app/bridge.rs:275`), so every
            // sign-in retry, sign-out and "Delete all local data" round trip mints a fresh
            // instance — and, before this arm existed, orphaned the outgoing one's GL texture on
            // every one of those visits: nothing ever deleted `qr_tex` for an instance that was
            // simply going away rather than replacing its OWN code. This is the same failure the
            // legacy single-`Scene` screen already had to name once — `ui/login.rs`'s
            // `drop_a_stale_qr` doc: "zeroing the handle alone orphaned a full 400x400-ish RGBA QR
            // bitmap per sign-in retry, with nothing left holding its id to free it later" — except
            // that comment guarded a re-upload inside one long-lived `Scene` across retries, which
            // never covered a whole INSTANCE being torn down, since the legacy screen had no such
            // thing. Freeing it in `Screen::prepare` (where the struct's own field doc says GL
            // resources are otherwise built and freed) is not an option: an unmounting instance
            // gets no further `prepare` call to do it in, so the free has to run wherever teardown
            // is actually observed, which for an owned `Screen` is here. `gfx::delete_tex` no-ops
            // on 0, so a screen that never uploaded a texture (never reached `Phase::Waiting`, or
            // reached it with no QR PNG yet) frees nothing.
            ScreenEvent::Unmount => {
                crate::gfx::delete_tex(self.qr_tex);
                self.qr_tex = 0;
                self.qr_px = (0, 0);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl<H: AuthLike> Screen<H> for LoginScreen {
    fn name(&self) -> &'static str {
        word::LOGIN
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> {
        // This is one of the family's three routes with nowhere for BACK to go INSIDE the app
        // (`ui/CLAUDE.md`'s route-family rule) — see this screen's BACK arm.
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {
        self.prepare_qr_tex();
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter;
        // The QR screen is the first thing a new user sees, before Home has any artwork to lend
        // it. `Painter::root()`, not `p`, for the ground — the ambient wash must not ride whatever
        // page-transition cascade this screen's own painter carries, exactly as
        // `screens::onboard`'s first-run mounting draws its own ground the same way.
        self.ground.draw_default(Painter::root());
        let env = Env::inert();
        let focus = f.focus.current.map(|k| k.elem);

        if self.persistence_warning.is_some() {
            // AUTH-03: reachable before consent/profile routing, exactly as the phase-keyed
            // branches below — see `app/run.rs`'s own routing gate for the mirror of this check.
            self.draw_warning(f, p, &env, focus);
        } else {
            match self.phase {
                Phase::Waiting => self.draw_waiting(f, p, focus),
                Phase::Error => self.draw_failed(f, p, &env, focus),
                Phase::Deleted => self.draw_deleted(f, p, &env, focus),
                Phase::Discovering => {
                    self.draw_working(f, p, &env, "Finding your server\u{2026}", focus)
                }
                _ => self.draw_working(f, p, &env, "Connecting to Plex\u{2026}", focus),
            }
        }
        self.draw_alert(f);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    /// The QR bitmap is the one render this screen owns (§8.3 rule (c)) — its own `upload_rgba` in
    /// [`Self::prepare_qr_tex`], its own `delete_tex` on `Unmount`. Everything else here is drawn
    /// immediate-mode or comes from a shared cache. The size is whatever plex.tv's PNG decoded to,
    /// which is why it is recorded rather than assumed.
    fn render_report(&self) -> crate::ui::frame::RenderReport {
        if self.qr_tex == 0 {
            return crate::ui::frame::RenderReport::NONE;
        }
        crate::ui::frame::RenderReport::one(self.qr_px.0, self.qr_px.1)
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    // **A click only lands inside the one control's own rect, never anywhere else on the screen —
    // a deliberate departure from the pre-phase-6 behaviour, not an oversight.** The legacy loop's
    // `Route::Login` click arm (`app/run.rs`, before 957bdc4d) fired
    // `crate::ui::login::key(SDLK_RETURN, 0)` on ANY click anywhere on this route, because that
    // screen predates real per-widget hit testing and "one actionable thing on the login screen"
    // was reason enough to skip building it; `key()` itself still gated on whether a control was
    // actually offered, so the only visible effect was that a tap on the QR image, the URL, or
    // empty space fired Retry/Sign-in/the stalled-wait escape whenever one happened to be showing.
    // That is exactly the failure mode real hit testing exists to rule out everywhere else in this
    // family (a tap near a poster's shadow must not activate a card three rows away).
    // `HitSource::Engine` resolves a click against `Focusable::place`'s own rect like every other
    // owned screen, so a tap has to land on the pill (or, on the QR screen, the status sentence)
    // to do anything. Restoring click-anywhere would be a step backward, not a port.
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, LazyLock};

    use crate::ui::consts::inside_safe;
    use crate::ui::machine::{
        FocusRead, Host, InputOwner, InstanceId, MachineId, PressRead, Source, Stamped,
    };
    use crate::ui::present::Present;

    struct SessionHost;

    impl Host for SessionHost {
        type Arg = super::super::family::SettingsPage;
        type Fx = AppFx;
        type Msg = AppMsg;
        type Elem = u32;
        type Views<'a> = auth::SessionRead<'a>;
        type Init = super::super::family::NoInit;
        type Memory = ();
    }

    impl AuthLike for SessionHost {
        fn auth<'a>(cx: &Cx<'a, Self>) -> auth::SessionRead<'a> {
            cx.views
        }
    }

    fn snapshot(phase: Phase, qr_generation: u64, code: &str) -> auth::owner::SessionSnapshot {
        auth::owner::SessionSnapshot {
            flow_epoch: 0,
            phase,
            qr_generation,
            code: Arc::from(code),
            png: Arc::from(Vec::<u8>::new()),
            code_replaced: false,
            users: Arc::from(Vec::<auth::UserTile>::new()),
            error: Arc::from(""),
            pin_denied: false,
            profile: None,
            scope: auth::owner::ProfileScope(0),
            delete_leftovers: 0,
            persistence_warning: None,
            incident: None,
            link_trouble: false,
            plaintext: None,
            switch_refused: false,
            readout_back_resumes: false,
        }
    }

    static EMPTY_SNAPSHOT: LazyLock<auth::owner::SessionSnapshot> =
        LazyLock::new(|| snapshot(Phase::Idle, 0, ""));

    /// **A partial wipe may not be reported as a whole one.** The sweep's candidate lists span
    /// both webOS install prefixes and the jail profiles disagree about which are writable, so a
    /// survivor is ordinary — and the survivor can be the TELEMETRY decision, which is then
    /// re-read on the next launch. Saying "telemetry has been removed" over that is the one
    /// sentence on this screen that could be actively false. Ported verbatim from `ui/login.rs`.
    #[test]
    fn uncertain_db8_reply_appears_on_the_login_warning_stage_line() {
        for reconcile in [false, true] {
            let outcome = crate::plex::session::persistence::uncertain_helper_reply_for_test(reconcile);
            let mut screen = bare_screen(Phase::Ready, 0.0);
            screen.persistence_warning = Some(auth::owner::PersistenceWarning::from_outcome(
                auth::owner::PersistenceWarningKey { epoch: 1, req: 1 },
                auth::owner::PersistenceWarningSite::Final, &outcome,
            ));
            assert_eq!(screen.report_note().unwrap().text.to_str().unwrap(), "storage: helper · db8 (-3963)");
        }
    }

    #[test]
    fn helper_failure_warning_line_is_only_for_helper_failures() {
        use crate::storage::wire::failure::{HelperFailure, Stage};
        let mut screen = bare_screen(Phase::Ready, 0.0);
        assert!(screen.report_note().is_none());
        screen.persistence_warning = Some(auth::owner::PersistenceWarning {
            key: auth::owner::PersistenceWarningKey { epoch: 1, req: 1 },
            site: auth::owner::PersistenceWarningSite::Final,
            helper: Some(HelperFailure { start_timeout: true,
                ..HelperFailure::new(Stage::RuntimeAbsent, Some(libc::ENOENT)) }),
            candidate_errnos: [None; 8], persistence: None,
        });
        assert!(screen.report_note().unwrap().text.to_string_lossy().contains("storage: start-timeout · no runtime dir"));
        screen.persistence_warning.as_mut().unwrap().helper = None;
        assert!(screen.report_note().is_none());
    }

    #[test]
    fn a_partial_wipe_does_not_claim_a_whole_one() {
        let (whole, whole_why) = deleted_readout(0);
        let (partial, partial_why) = deleted_readout(2);
        assert_ne!(whole, partial);
        assert!(whole_why.to_bytes().windows(9).any(|w| w == b"telemetry"));
        assert!(
            !partial_why.to_bytes().windows(9).any(|w| w == b"telemetry"),
            "a partial wipe must not name what it may have failed to delete"
        );
        assert!(
            partial.to_bytes().windows(10).any(|w| w == b"Signed out"),
            "…but it still states what it DID do: the session is gone either way"
        );
    }

    /// **A stalled sign-in has to be escapable, and until 2026-09-02 it was not.** The control
    /// appears on a clock, so the whole rule is a pure predicate.
    ///
    /// **Pins the DESIGN NUMBER itself, not just the `>=` operator.** Every boundary used to be
    /// phrased in terms of `ESCAPE_AFTER_MS` (`ESCAPE_AFTER_MS - 1.0`, `ESCAPE_AFTER_MS`), so a
    /// mutation that changed the constant's value — the reviewer tried `120.0` — left this test
    /// green regardless: it was grading the comparison, not the twelve seconds. The literal
    /// `assert_eq!` and the literal millisecond boundaries below close that gap.
    #[test]
    fn a_wait_that_stops_looking_normal_grows_a_way_out() {
        assert_eq!(
            ESCAPE_AFTER_MS, 12_000.0,
            "the documented twelve-second design number"
        );
        assert!(!escape_offered(0.0), "a fresh wait offers nothing");
        assert!(
            !escape_offered(11_999.0),
            "nor does a healthy one — a button that flashes past teaches people to ignore it"
        );
        assert!(escape_offered(12_000.0));
    }

    /// **The escape belongs ONLY to the two phases that wait on a network call.** Ported verbatim.
    #[test]
    fn only_a_phase_waiting_on_the_network_can_be_stalled() {
        assert!(working_phase(Phase::Creating));
        assert!(working_phase(Phase::Discovering));
        for settled in [
            Phase::Idle,
            Phase::Waiting,
            Phase::Profiles,
            Phase::Switching,
            Phase::Ready,
            Phase::Error,
            Phase::Deleted,
        ] {
            assert!(
                !working_phase(settled),
                "{settled:?} is not a wait this screen may offer to restart"
            );
        }
    }

    /// **A code that has been replaced may not go on being drawn**, which is the login screen's
    /// half of issue #30. Ported verbatim.
    #[test]
    fn a_replaced_code_invalidates_the_cached_qr_even_without_a_phase_change() {
        assert!(
            qr_cache_stale(4, 5, Phase::Waiting),
            "a new code was published while the screen never left Waiting"
        );
        assert!(
            !qr_cache_stale(5, 5, Phase::Waiting),
            "…and the settled case must not re-upload a texture every frame"
        );
        assert!(qr_cache_stale(5, 5, Phase::Creating));
    }

    /// The one line under the code has to explain a swap the user did not ask for. Ported
    /// verbatim.
    #[test]
    fn a_swapped_code_says_so_rather_than_changing_under_the_user() {
        let says = |s: &CStr, word: &[u8]| s.to_bytes().windows(word.len()).any(|w| w == word);
        assert!(says(waiting_status(false, false, false), b"Waiting"));
        assert!(
            says(waiting_status(true, false, false), b"expired"),
            "it names what happened; a code that simply changes reads as a fault"
        );
        assert!(says(waiting_status(true, true, false), b"press OK"));
        assert!(says(waiting_status(false, true, false), b"press OK"));
        // While plex.tv is not answering at all, neither a scan nor a new code can work: the line
        // says where the fault most likely is, whatever else is true.
        for (replaced, stalled) in [(false, false), (true, false), (false, true), (true, true)] {
            assert!(says(waiting_status(replaced, stalled, true), b"internet connection"));
        }
    }

    /// **The QR screen's clock is not the spinner's, and it must not be.**
    ///
    /// **Pins both DESIGN NUMBERS, not just their ratio.** The relative check
    /// (`QR_ESCAPE_AFTER_MS > ESCAPE_AFTER_MS * 4.0`) survives mutating BOTH constants together —
    /// the reviewer's example, `ESCAPE_AFTER_MS = 120.0` and `QR_ESCAPE_AFTER_MS = 600.0`, still
    /// satisfies `600 > 120 * 4`. The `assert_eq!`s and the literal millisecond boundaries below
    /// pin sixty seconds and twelve seconds as VALUES, so that mutation fails here even though the
    /// ratio it preserves would not have caught it.
    #[test]
    fn the_qr_screen_offers_a_new_code_on_a_much_longer_clock_than_a_stalled_spinner() {
        assert_eq!(ESCAPE_AFTER_MS, 12_000.0);
        assert_eq!(QR_ESCAPE_AFTER_MS, 60_000.0);
        assert!(QR_ESCAPE_AFTER_MS > ESCAPE_AFTER_MS * 4.0);
        assert!(!qr_escape_offered(0.0));
        assert!(
            !qr_escape_offered(12_000.0),
            "a sign-in that is merely twelve seconds old is going fine"
        );
        assert!(
            QR_ESCAPE_AFTER_MS < 900_000.0,
            "…and it must arrive well inside a code's own fifteen-minute life, or it is not a \
             recovery from anything"
        );
        assert!(!qr_escape_offered(59_999.0));
        assert!(qr_escape_offered(60_000.0));
    }

    /// **A new code starts a new clock, even if the phase change between them was never sampled.**
    /// Ported verbatim.
    #[test]
    fn a_replaced_code_restarts_the_wait_even_when_the_phase_never_appeared_to_change() {
        let old_code: Wait = (Phase::Waiting, 7u64);
        assert!(
            !wait_restarted(old_code, (Phase::Waiting, 7)),
            "the same code in the same phase is the same wait, and the clock must keep running"
        );
        assert!(
            wait_restarted(old_code, (Phase::Waiting, 8)),
            "a new code is a new wait, whatever the phase appeared to do in between — this is \
             the case a phase-only comparison misses, and it hands a one-millisecond-old code \
             its predecessor's sixty seconds"
        );
        assert!(
            wait_restarted(old_code, (Phase::Discovering, 7)),
            "…and the original rule still holds: a step forward is a fresh wait"
        );
    }

    /// Ported verbatim.
    #[test]
    fn qr_is_vertically_centred_and_the_whole_link_stack_stays_in_the_right_column() {
        let route = RouteLayout::screen();
        let q = qr_layout(route);
        assert_eq!(q.card.cy(), Rect::FULL.cy());
        for r in [q.url, q.card, q.code, q.status] {
            assert!(r.x >= route.content.x);
            assert!(r.x + r.w <= route.content.x + route.content.w);
            assert!(inside_safe(r));
        }
    }

    /// A bare screen, built with NO read of `crate::auth` at all — for testing [`ControlKind`]'s
    /// decision and the `Focusable` geometry without consulting even the local test host's
    /// Session publication.
    fn bare_screen(phase: Phase, phase_ms: f32) -> LoginScreen {
        LoginScreen {
            entry: EntryId(0),
            spin_ms: 0.0,
            phase_ms,
            spin_phase: crate::ui::motion::Phase::default(),
            phase_clock: crate::ui::motion::Phase::default(),
            wait: (phase, 0),
            qr_tex: 0,
            qr_tex_gen: 0,
            qr_px: (0, 0),
            qr_png_pending: None,
            phase,
            qr_gen: 0,
            qr_code: Arc::from(""),
            qr_replaced: false,
            error: Arc::from(""),
            delete_leftovers: 0,
            next_correlation: Some(1),
            pending_restart: None,
            persistence_warning: None,
            plaintext: None,
            question: PlaintextQuestion::new(),
            ground: RouteGround::new(),
            report: Report::new(),
            state: LoginState {
                phase: 0,
                qr_gen: 0,
                qr_replaced: false,
                has_control: false,
                delete_leftovers: 0,
                next_correlation: Some(1),
                pending_restart: None,
                warning: false,
                report: ReportState::default(),
                plaintext: None,
            },
        }
    }

    /// **The one property this port exists to protect**: a control that is not drawn must not be
    /// activatable, restated for the owned screen as "a control exists only in exactly the phases
    /// `ui/login.rs`'s `key()` ladder ever acted on". `escape_ready`/`qr_escape_ready`'s old bodies
    /// are gone; this is the test that pins the same property against their replacement,
    /// `LoginScreen::control_kind`.
    ///
    /// **The boundaries below are literal milliseconds, not `ESCAPE_AFTER_MS`/`QR_ESCAPE_AFTER_MS`
    /// themselves.** Feeding a threshold its OWN constant as input proves only that `control_kind`
    /// compares two copies of the same symbol — it stays green under any value that constant takes,
    /// including the reviewer's mutated `ESCAPE_AFTER_MS = 120.0`. Literal `11_999.0`/`12_000.0` and
    /// `59_999.0`/`60_000.0` pin the twelve- and sixty-second design numbers themselves, matching
    /// the two tests above that pin the same constants directly.
    #[test]
    fn the_control_exists_only_where_legacy_ever_acted_on_a_press() {
        assert_eq!(bare_screen(Phase::Waiting, 0.0).control_kind(), None);
        assert_eq!(bare_screen(Phase::Waiting, 59_999.0).control_kind(), None);
        assert_eq!(
            bare_screen(Phase::Waiting, 60_000.0).control_kind(),
            Some(ControlKind::RestartWait)
        );
        assert_eq!(bare_screen(Phase::Creating, 0.0).control_kind(), None);
        assert_eq!(bare_screen(Phase::Creating, 11_999.0).control_kind(), None);
        assert_eq!(
            bare_screen(Phase::Creating, 12_000.0).control_kind(),
            Some(ControlKind::RestartWait)
        );
        assert_eq!(bare_screen(Phase::Discovering, 0.0).control_kind(), None);
        assert_eq!(
            bare_screen(Phase::Discovering, 11_999.0).control_kind(),
            None
        );
        assert_eq!(
            bare_screen(Phase::Discovering, 12_000.0).control_kind(),
            Some(ControlKind::RestartWait)
        );
        // Error and Deleted offer their control unconditionally — no clock involved.
        assert_eq!(
            bare_screen(Phase::Error, 0.0).control_kind(),
            Some(ControlKind::Retry)
        );
        assert_eq!(
            bare_screen(Phase::Deleted, 0.0).control_kind(),
            Some(ControlKind::StartLogin)
        );
        // The allowlist `working_phase` states explicitly: a phase the main loop is about to route
        // away from must never grow an escape that could call `auth::retry` on a flow that already
        // succeeded.
        for settled in [Phase::Idle, Phase::Profiles, Phase::Switching, Phase::Ready] {
            assert_eq!(
                bare_screen(settled, 1_000_000.0).control_kind(),
                None,
                "{settled:?}"
            );
        }
    }

    fn cx_with<'a>(
        m: &'a crate::ui::fixture::FixtureMeasure,
        snapshot: &'a auth::owner::SessionSnapshot,
    ) -> Cx<'a, SessionHost> {
        Cx {
            views: snapshot.read(),
            tick: crate::ui::machine::Tick::default(),
            measure: m,
            press: PressRead::default(),
            focus: FocusRead {
                current: None,
                ..Default::default()
            },
            owner: InputOwner::Entry(EntryId(0)),
        }
    }

    fn test_cx<'a>(m: &'a crate::ui::fixture::FixtureMeasure) -> Cx<'a, SessionHost> {
        cx_with(m, &EMPTY_SNAPSHOT)
    }

    /// The focus GROUP exists exactly when [`LoginScreen::has_control`] does, and its one element
    /// is `ElemKind::Bare` — the escape acts on the key-down edge with no hold and no press dip,
    /// like every read-out action in this family, never a `Control`/`Card`.
    #[test]
    fn the_control_group_exists_only_while_a_control_is_offered() {
        let m = crate::ui::fixture::FixtureMeasure;
        let cx = test_cx(&m);

        let quiet = bare_screen(Phase::Waiting, 0.0);
        let mut groups = Vec::new();
        Focusable::<SessionHost>::groups(&quiet, &cx, &mut groups);
        assert!(
            groups.is_empty(),
            "nothing to press while the code is fresh"
        );
        assert!(Focusable::<SessionHost>::place(&quiet, &CONTROL, &cx, At::Drawn).is_none());
        assert!(Focusable::<SessionHost>::group_of(&quiet, &CONTROL, &cx).is_none());

        let stuck = bare_screen(Phase::Waiting, QR_ESCAPE_AFTER_MS);
        groups.clear();
        Focusable::<SessionHost>::groups(&stuck, &cx, &mut groups);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].elem, ElemKind::Bare);
        assert_eq!(groups[0].id, CONTROL_GROUP);
        assert!(Focusable::<SessionHost>::place(&stuck, &CONTROL, &cx, At::Drawn).is_some());
        assert_eq!(
            Focusable::<SessionHost>::group_of(&stuck, &CONTROL, &cx),
            Some(CONTROL_GROUP)
        );
    }

    /// A stalled Working read-out draws its action pill LOWER than a settled one carrying the same
    /// reason — `StatusOverlay::bands`'s `Working` branch offsets the caption down from the frame
    /// centre instead of straddling it, and this is the one number that formula depends on that a
    /// host test can actually see (the caption/reason heights come from [`FixtureMeasure`], not a
    /// loaded font, so this pins the ORDERING the two `cap_y` branches produce, not literal
    /// on-device pixels).
    ///
    /// **Trimmed to the one assertion that is actually about this ORDERING.** This test used to
    /// also carry `assert_eq!(working.h, StatusOverlay::CTRL_H)` and a centering check — both
    /// restate a literal `status_action_rect` writes into its own return unconditionally
    /// (`Rect::new(frame.cx() - w * 0.5, …, w, StatusOverlay::CTRL_H)` centres on `frame.cx()` and
    /// carries `CTRL_H` for ANY `w`, by construction, whatever the surrounding logic does), so
    /// they could not fail no matter how broken the geometry got. The property they were reaching
    /// for — that this hand-reproduced Rect actually agrees with the widget it reproduces — is the
    /// real question, and it needed a real widget on the other side of the assertion:
    /// [`status_action_rect_matches_the_widget_it_is_reproducing`] below is that test.
    #[test]
    fn a_stalled_working_readout_sits_its_action_pill_lower_than_a_settled_one() {
        let m = crate::ui::fixture::FixtureMeasure;
        let working = status_action_rect(&m, ESCAPE, StatusKind::Working, true);
        let settled = status_action_rect(&m, ESCAPE, StatusKind::Empty, true);
        assert!(
            working.y > settled.y,
            "Working straddles the centre with the spinner above it; a settled read-out centres \
             the caption alone, which sits higher"
        );
    }

    /// A reason line pushes the action pill further down — `bands`'s `below` accumulator, ported
    /// onto `Measure` — for a centred read-out and for the page-filling `Failed` one alike: the
    /// row always stacks under the copy.
    #[test]
    fn a_reason_line_pushes_the_action_pill_down_further() {
        let m = crate::ui::fixture::FixtureMeasure;
        for kind in [StatusKind::Working, StatusKind::Empty, StatusKind::Failed] {
            let with_reason = status_action_rect(&m, ESCAPE, kind, true);
            let without = status_action_rect(&m, ESCAPE, kind, false);
            assert!(with_reason.y > without.y, "{kind:?}");
        }
    }

    /// **The failed read-out stands on the page read-out's anchor and never grows.** Its verdict
    /// hangs from `StatusOverlay::FULL_ANCHOR_TOP` and its row — *Try again* / *Details*, nothing
    /// else — stacks `space::LG` under the reason; a report's status line, a sendable offer and a
    /// delivered report all leave the row exactly where it was.
    #[test]
    fn the_failed_readout_stands_on_the_page_lines_and_never_grows() {
        use auth::owner::IncidentState as S;
        let m = crate::ui::fixture::FixtureMeasure;
        let pin = crate::telemetry::incident::IncidentKind::PinCreate;
        let rows = |s: &LoginScreen| {
            let (labels, has_reason) = s.readout_labels();
            status_row_rects(&m, labels, s.readout_kind(), has_reason)
        };
        let calm = rows(&screen_with(Phase::Error, S::NotNow, pin));
        let (primary, details) = (calm[0].unwrap(), calm[1].unwrap());
        let failed = screen_with(Phase::Error, S::NotNow, pin);
        let (labels, has_reason) = failed.readout_labels();
        assert!(has_reason, "the sign-in failure always says why");
        let overlay = readout_overlay(c"", failed.readout_kind(), Some(c""), labels, None);
        let verdict = overlay.verdict_band_measured(&m);
        assert_eq!(verdict.y, StatusOverlay::FULL_ANCHOR_TOP);
        assert_eq!(overlay.action_frame_measured(&m).unwrap().y, primary.y, "the drawn row is the hit row");
        assert!(primary.y > verdict.y + verdict.h, "the row stacks under the copy");
        assert_eq!(details.y, primary.y, "Details shares the row");
        for state in [S::Offered { revision: 0 }, S::Sending, S::Delivered { receipt: "0123abcd".into() }] {
            let s = screen_with(Phase::Error, state.clone(), pin);
            let r = rows(&s);
            assert_eq!((r[0].unwrap().y, r[1].unwrap().x), (primary.y, details.x), "{state:?}");
            assert_eq!(s.row().as_slice(), [CONTROL, DETAILS], "{state:?}: two controls, never three");
        }
    }

    /// A [`Measure`] that forwards straight to `crate::text`'s own raw, no-font-loaded fallbacks —
    /// the exact numbers `StatusOverlay::bands`/`action_frame` themselves fall back to when nothing
    /// has called `init_text`, which is true of every host test. It exists ONLY for the test below,
    /// and neither of the two `Measure`s already in this file can stand in for it:
    /// [`crate::ui::fixture::FixtureMeasure`]'s `line_h` is `sz * 1.2`, a deliberately rough
    /// stand-in for real font metrics (see its own doc) rather than the widget's actual fallback —
    /// `text_height`'s is `sz` exactly, factor 1.0, no font needed — so a `FixtureMeasure`-based
    /// comparison against the real widget could never agree numerically even when the two formulas
    /// are identical; and `TtfMeasure::width` carries a `debug_assert!` that exists to catch a REAL
    /// draw reached before boot's `init_text`, which every host test would trip on the first call.
    /// Neither omission was a bug — `FixtureMeasure` is for tests that only need INTERNAL
    /// consistency (the two tests above), and `TtfMeasure`'s assert is doing its job — but their
    /// combination is exactly why the property `status_action_rect`'s own doc claims ("mirrors …
    /// exactly") had never actually been checked by anything in this file.
    struct RawTextMeasure;
    impl Measure for RawTextMeasure {
        fn width(&self, s: &CStr, sz: i32, bold: bool) -> f32 {
            crate::text::text_width(s.as_ptr(), sz, bold as c_int)
        }
        fn cap_h(&self, sz: i32) -> f32 {
            crate::text::cap_h(sz, 0)
        }
        fn line_h(&self, sz: i32) -> f32 {
            crate::text::text_height(sz, 0)
        }
    }

    /// **The property `status_action_rect`'s own doc claims, actually checked.** Neither test above
    /// it compares against a real [`StatusOverlay`] at all — both only compare two calls to
    /// `status_action_rect` against EACH OTHER, which can pin an ordering but can never catch the
    /// two formulas drifting apart from each other, term for term, in lockstep. This test builds a
    /// real `StatusOverlay` with the same shape (`kind`, `reason`, `action`) `LoginScreen::draw`
    /// ever configures one with and asserts its `action_frame()` Rect is bit-for-bit the Rect
    /// `status_action_rect` computes for the equivalent inputs, across both axes
    /// (`working`/`has_reason`) this screen ever combines. `RawTextMeasure` is what makes an exact
    /// comparison possible on the host at all — see its own doc for why `FixtureMeasure` cannot do
    /// this job.
    #[test]
    fn status_action_rect_matches_the_widget_it_is_reproducing() {
        for kind in [StatusKind::Working, StatusKind::Failed, StatusKind::Empty] {
            for has_reason in [false, true] {
                let mut o = StatusOverlay::new(Rect::FULL, c"caption", kind).page().action(ESCAPE);
                if has_reason {
                    o = o.reason(c"This is taking longer than usual.");
                }
                let want = o.action_frame().expect("an action was set above");
                let got = status_action_rect(&RawTextMeasure, ESCAPE, kind, has_reason);
                assert_eq!(
                    (got.x, got.y, got.w, got.h),
                    (want.x, want.y, want.w, want.h),
                    "kind={kind:?} has_reason={has_reason}"
                );
            }
        }
    }

    /// **The frozen-animator regression class, closed for this screen's clock (phase 12 D4).**
    /// `spin_ms`/`phase_ms` used to be raw `+= dt` accumulators with a SEPARATE, easy-to-forget
    /// `fx.note(Motion)` a few lines below them — exactly the shape `Xfade`/`Spinner` shipped
    /// frozen in before (`docs/agent-reference.md`'s idle section). Now both are `motion::Phase`,
    /// which reports from inside its own `advance`, so this proves the report survives the
    /// refactor: three real `Tick`s in a row, driven through `Machine::step` exactly as the loop
    /// drives one, must each present. (The complementary "a settled read-out's clock does not
    /// report" half is `control_has_spinner`'s own predicate, unchanged by this conversion, and is
    /// not re-driven here through `resync`; the pure predicate already pins that settled half.)
    #[test]
    fn the_spinner_phase_reports_motion_on_every_tick_while_a_control_has_one() {
        let mut s = LoginScreen::new(EntryId(0), EMPTY_SNAPSHOT.read());
        let m = crate::ui::fixture::FixtureMeasure;
        let cx = test_cx(&m);
        let mut present = Present::new();
        let _ = present.take(0);
        let mut buf: Vec<Stamped<SessionHost>> = Vec::new();
        for ms in [16, 32, 48] {
            let mut fx = Effects::new(&mut buf, MachineId::Instance(InstanceId(0)), &mut present);
            let ev = ScreenEvent::Tick(Tick { ms, dt_us: 16_667 });
            Machine::<SessionHost>::step(&mut s, &ev, &cx, &mut fx);
            assert!(
                present.take(ms),
                "a live sign-in spinner must present every frame it is on screen (ms={ms})"
            );
        }
    }

    fn step_ev(
        s: &mut LoginScreen,
        ev: &ScreenEvent<SessionHost>,
    ) -> (Handled, Vec<Stamped<SessionHost>>) {
        let m = crate::ui::fixture::FixtureMeasure;
        step_ev_with(s, ev, &EMPTY_SNAPSHOT, InstanceId(0), &m)
    }

    fn step_ev_with(
        s: &mut LoginScreen,
        ev: &ScreenEvent<SessionHost>,
        snapshot: &auth::owner::SessionSnapshot,
        instance: InstanceId,
        m: &crate::ui::fixture::FixtureMeasure,
    ) -> (Handled, Vec<Stamped<SessionHost>>) {
        let _frame_scope = crate::task::FrameScope::enter();
        let cx = cx_with(m, snapshot);
        let mut present = Present::new();
        let mut buf: Vec<Stamped<SessionHost>> = Vec::new();
        let handled = {
            let mut fx = Effects::new(&mut buf, MachineId::Instance(instance), &mut present);
            Machine::<SessionHost>::step(s, ev, &cx, &mut fx)
        };
        (handled, buf)
    }

    fn key_back_down() -> ScreenEvent<SessionHost> {
        ScreenEvent::Input(InputEvent {
            at: crate::ui::machine::Tick::default(),
            source: Source::Script,
            kind: InputKind::Key {
                key: Key::Back,
                // the remote's own BACK code, so the alert trap (which classifies the raw press
                // to tell BACK from Stop and Exit) reads it too
                sym: 0,
                wcode: crate::ui::consts::WCODE_BACK,
                edge: Edge::Down,
                at_edge: false,
            },
        })
    }

    #[test]
    fn constructor_and_ticks_retain_coherent_host_publications_per_instance() {
        let mut first_snapshot = snapshot(Phase::Waiting, 41, "AAAA");
        first_snapshot.png = Arc::from(vec![1, 2, 3]);
        let mut second_snapshot = snapshot(Phase::Error, 99, "BBBB");
        second_snapshot.error = Arc::from("second failed");

        let first = LoginScreen::new(EntryId(1), first_snapshot.read());
        let mut second = LoginScreen::new(EntryId(2), second_snapshot.read());
        assert_eq!(
            (first.phase, first.qr_gen, first.qr_code.as_ref()),
            (Phase::Waiting, 41, "AAAA")
        );
        assert_eq!(
            first.qr_png_pending.as_ref().map(|(_, png)| png.as_ref()),
            Some(&[1, 2, 3][..])
        );
        assert_eq!(
            (second.phase, second.error.as_ref()),
            (Phase::Error, "second failed")
        );

        let replacement = snapshot(Phase::Waiting, 100, "CCCC");
        let m = crate::ui::fixture::FixtureMeasure;
        step_ev_with(
            &mut second,
            &ScreenEvent::Tick(Tick {
                ms: 16,
                dt_us: 16_667,
            }),
            &replacement,
            InstanceId(2),
            &m,
        );

        assert_eq!(
            (second.phase, second.qr_gen, second.qr_code.as_ref()),
            (Phase::Waiting, 100, "CCCC")
        );
        assert_eq!(
            (first.phase, first.qr_gen, first.qr_code.as_ref()),
            (Phase::Waiting, 41, "AAAA")
        );
    }

    #[test]
    fn controls_emit_typed_session_commands_without_mutating_the_wait_clock() {
        let mut retry = bare_screen(Phase::Error, 0.0);
        let (_, retry_fx) = step_ev(&mut retry, &ScreenEvent::Activate(CONTROL));
        assert!(retry_fx
            .iter()
            .any(|st| matches!(st.fx, Fx::App(AppFx::Session(auth::SessionCmd::Retry)))));

        let mut start = bare_screen(Phase::Deleted, 0.0);
        let (_, start_fx) = step_ev(&mut start, &ScreenEvent::Activate(CONTROL));
        assert!(start_fx
            .iter()
            .any(|st| matches!(st.fx, Fx::App(AppFx::Session(auth::SessionCmd::StartLogin)))));

        let mut restart = bare_screen(Phase::Waiting, QR_ESCAPE_AFTER_MS);
        restart.wait = (Phase::Waiting, 7);
        restart.qr_gen = 7;
        let m = crate::ui::fixture::FixtureMeasure;
        let published = snapshot(Phase::Waiting, 7, "AAAA");
        let (_, restart_fx) = step_ev_with(
            &mut restart,
            &ScreenEvent::Activate(CONTROL),
            &published,
            InstanceId(44),
            &m,
        );
        assert_eq!(
            restart.phase_ms, QR_ESCAPE_AFTER_MS,
            "emission is not acceptance"
        );
        assert!(restart_fx.iter().any(|st| matches!(
            &st.fx,
            Fx::App(AppFx::Session(auth::SessionCmd::RestartWait {
                phase: Phase::Waiting,
                qr_generation: 7,
                reply,
            })) if reply.instance == 44 && reply.correlation == 1
        )));
    }

    /// Regression for `warning-routing-and-continue-untested`. Pins the UI half of the AUTH-03
    /// gate: while `persistence_warning` is showing, the one offered control is `ContinueUnsaved`,
    /// and activating it emits EXACTLY one `AcknowledgePersistenceWarning` carrying the shown
    /// warning's OWN key — never `Retry`/`StartLogin`/nothing. MUTATION for this finding: make
    /// `LoginScreen::activate`'s `Some(ControlKind::ContinueUnsaved)` arm push nothing (or push the
    /// wrong key) — this test must then fail, because acknowledging is the only door that can ever
    /// release a held Final handoff (`auth/owner.rs::acknowledge_persistence_warning`), so a
    /// no-op here strands the user in front of the warning forever.
    #[test]
    fn the_warning_screens_one_control_acknowledges_exactly_that_warning() {
        let key = auth::owner::PersistenceWarningKey { epoch: 3, req: 9 };
        let warning = auth::owner::PersistenceWarning {
            key,
            site: auth::owner::PersistenceWarningSite::Final,
            helper: None, candidate_errnos: [None; 8], persistence: None,
        };
        let mut screen = bare_screen(Phase::Ready, 0.0);
        screen.persistence_warning = Some(warning);
        assert_eq!(
            screen.control_kind(),
            Some(ControlKind::ContinueUnsaved),
            "a live warning is the only control offered, regardless of the underlying phase"
        );

        let (_, effects) = step_ev(&mut screen, &ScreenEvent::Activate(CONTROL));
        let acks: Vec<_> = effects
            .iter()
            .filter(|st| {
                matches!(
                    st.fx,
                    Fx::App(AppFx::Session(auth::SessionCmd::AcknowledgePersistenceWarning { .. }))
                )
            })
            .collect();
        assert_eq!(acks.len(), 1, "exactly one acknowledgement, never zero and never a second");
        assert!(
            matches!(
                acks[0].fx,
                Fx::App(AppFx::Session(auth::SessionCmd::AcknowledgePersistenceWarning { key: acked }))
                    if acked == key
            ),
            "the acknowledgement must carry the SHOWN warning's own key, not a stale or default one"
        );
        assert!(
            !effects.iter().any(|st| matches!(
                st.fx,
                Fx::App(AppFx::Session(auth::SessionCmd::Retry | auth::SessionCmd::StartLogin))
            )),
            "activating the warning control must never also fire an unrelated session command"
        );
    }

    #[test]
    fn only_the_matching_accepted_restart_reply_resets_the_stalled_wait() {
        let mut screen = bare_screen(Phase::Waiting, QR_ESCAPE_AFTER_MS);
        screen.wait = (Phase::Waiting, 7);
        screen.qr_gen = 7;
        let published = snapshot(Phase::Waiting, 7, "AAAA");
        let m = crate::ui::fixture::FixtureMeasure;
        step_ev_with(
            &mut screen,
            &ScreenEvent::Activate(CONTROL),
            &published,
            InstanceId(44),
            &m,
        );
        assert_eq!(screen.pending_restart.map(|p| p.correlation), Some(1));

        step_ev_with(
            &mut screen,
            &ScreenEvent::Async(
                crate::ui::machine::RequestId(1),
                AppMsg::RestartReply {
                    correlation: 1,
                    accepted: false,
                },
            ),
            &published,
            InstanceId(44),
            &m,
        );
        assert_eq!(screen.phase_ms, QR_ESCAPE_AFTER_MS);
        assert!(
            screen.pending_restart.is_none(),
            "a matching refusal is terminal"
        );

        step_ev_with(
            &mut screen,
            &ScreenEvent::Activate(CONTROL),
            &published,
            InstanceId(44),
            &m,
        );
        assert_eq!(screen.pending_restart.map(|p| p.correlation), Some(2));
        step_ev_with(
            &mut screen,
            &ScreenEvent::Async(
                crate::ui::machine::RequestId(99),
                AppMsg::RestartReply {
                    correlation: 2,
                    accepted: true,
                },
            ),
            &published,
            InstanceId(44),
            &m,
        );
        assert_eq!(
            screen.phase_ms, QR_ESCAPE_AFTER_MS,
            "a foreign request id is not acceptance"
        );
        assert_eq!(screen.pending_restart.map(|p| p.correlation), Some(2));

        step_ev_with(
            &mut screen,
            &ScreenEvent::Async(
                crate::ui::machine::RequestId(2),
                AppMsg::RestartReply {
                    correlation: 2,
                    accepted: true,
                },
            ),
            &published,
            InstanceId(44),
            &m,
        );
        assert_eq!(screen.phase_ms, 0.0);
        assert!(screen.pending_restart.is_none());

        screen.phase_ms = 321.0;
        step_ev_with(
            &mut screen,
            &ScreenEvent::Async(
                crate::ui::machine::RequestId(2),
                AppMsg::RestartReply {
                    correlation: 2,
                    accepted: true,
                },
            ),
            &published,
            InstanceId(44),
            &m,
        );
        assert_eq!(
            screen.phase_ms, 321.0,
            "a duplicate accepted reply is stale"
        );
    }

    #[test]
    fn phase_progress_retires_a_carried_restart_reply_and_exhaustion_emits_nothing() {
        let waiting = snapshot(Phase::Waiting, 7, "AAAA");
        let progressed = snapshot(Phase::Discovering, 7, "");
        let m = crate::ui::fixture::FixtureMeasure;
        let mut screen = bare_screen(Phase::Waiting, QR_ESCAPE_AFTER_MS);
        screen.wait = (Phase::Waiting, 7);
        screen.qr_gen = 7;
        step_ev_with(
            &mut screen,
            &ScreenEvent::Activate(CONTROL),
            &waiting,
            InstanceId(44),
            &m,
        );
        step_ev_with(
            &mut screen,
            &ScreenEvent::Tick(Tick {
                ms: 16,
                dt_us: 16_667,
            }),
            &progressed,
            InstanceId(44),
            &m,
        );
        assert!(screen.pending_restart.is_none());
        screen.phase_ms = 222.0;
        step_ev_with(
            &mut screen,
            &ScreenEvent::Async(
                crate::ui::machine::RequestId(1),
                AppMsg::RestartReply {
                    correlation: 1,
                    accepted: true,
                },
            ),
            &progressed,
            InstanceId(44),
            &m,
        );
        assert_eq!(
            screen.phase_ms, 222.0,
            "the old wait's carried reply is stale"
        );

        let mut exhausted = bare_screen(Phase::Waiting, QR_ESCAPE_AFTER_MS);
        exhausted.wait = (Phase::Waiting, 7);
        exhausted.next_correlation = Some(u32::MAX);
        let (_, effects) = step_ev_with(
            &mut exhausted,
            &ScreenEvent::Activate(CONTROL),
            &waiting,
            InstanceId(44),
            &m,
        );
        assert!(effects
            .iter()
            .all(|st| !matches!(st.fx, Fx::App(AppFx::Session(_)))));
        assert!(exhausted.pending_restart.is_none());
        assert_eq!(exhausted.phase_ms, QR_ESCAPE_AFTER_MS);
    }

    /// **This screen has no panel of its own**, so every BACK is the typed Session root request.
    #[test]
    fn back_is_always_the_root_press() {
        let mut s = bare_screen(Phase::Waiting, 0.0);
        let (handled, effs) = step_ev(&mut s, &key_back_down());
        assert_eq!(handled, Handled::Yes);
        assert!(
            effs.iter().any(|st| matches!(
                &st.fx,
                Fx::App(AppFx::Session(auth::SessionCmd::BackAtRoot { reply }))
                    if reply.instance == 0 && reply.correlation == 1
            )),
            "login has no panel of its own to close first — BACK asks Session for the root press"
        );
    }

    /// The old route classifier had a dedicated `Switching` arm. The owned screen's decision is
    /// phase-independent, so pin that replacement with a real Switching publication and the
    /// addressed instance/correlation Session receives.
    #[test]
    fn back_during_switching_is_still_the_owned_logins_root_press() {
        let switching = snapshot(Phase::Switching, 17, "");
        let mut s = LoginScreen::new(EntryId(9), switching.read());
        let m = crate::ui::fixture::FixtureMeasure;
        let (handled, effects) = step_ev_with(&mut s, &key_back_down(), &switching,
            InstanceId(44), &m);
        assert_eq!(handled, Handled::Yes);
        assert!(effects.iter().any(|stamped| matches!(
            &stamped.fx,
            Fx::App(AppFx::Session(auth::SessionCmd::BackAtRoot { reply }))
                if reply.instance == 44 && reply.correlation == 1
        )), "Switching must not turn Login BACK into a local dismissal or an ignored key");
    }

    /// **The QR texture must not survive its screen — the leak `ScreenEvent::Unmount`'s own arm
    /// doc explains.** `999` stands in for a real GL id: a host test never uploads one for real
    /// (`prepare_qr_tex`'s own doc — GL work happens in `Screen::prepare`, never in `step`), so
    /// this pins the ONE property that matters without touching GL — that `step` zeroes `qr_tex`
    /// on `Unmount` regardless of what it held. Delete that arm, or let it fall back through to
    /// the catch-all `_ => Handled::No` the way it did before this fix, and this goes red:
    /// `qr_tex` stays `999` and `handled` reads `Handled::No`.
    #[test]
    fn unmount_frees_the_qr_texture() {
        let mut s = bare_screen(Phase::Waiting, 0.0);
        s.qr_tex = 999;
        s.qr_px = (400, 400);
        let (handled, _) = step_ev(&mut s, &ScreenEvent::Unmount);
        assert_eq!(handled, Handled::Yes);
        assert_eq!(
            s.qr_tex, 0,
            "an unmounting screen must not leak its GL texture id"
        );
        assert_eq!(
            s.qr_px,
            (0, 0),
            "…nor go on claiming its bytes in the frame's render set"
        );
    }

    /// **The QR bitmap is a render this SCREEN owns** — its own `upload_rgba`, its own
    /// `delete_tex` — so it is one of the two things in the tree that override
    /// `Screen::render_report` (§8.3 rule (c)); everything else on screen here is drawn
    /// immediate-mode or comes from a shared pool. `999` stands in for a real GL id for the same
    /// reason `unmount_frees_the_qr_texture` uses it: a host test uploads nothing.
    #[test]
    fn the_qr_bitmap_is_reported_as_this_screens_own_render() {
        use crate::ui::frame::RenderReport;
        let mut s = bare_screen(Phase::Waiting, 0.0);
        assert_eq!(
            Screen::<SessionHost>::render_report(&s),
            RenderReport::NONE,
            "no code yet: this screen holds no render of its own"
        );
        s.qr_tex = 999;
        s.qr_px = (400, 400);
        assert_eq!(
            Screen::<SessionHost>::render_report(&s),
            RenderReport::one(400, 400),
            "one texture, 400x400 RGBA8 = 640,000 bytes"
        );
    }

    fn incident(state: auth::owner::IncidentState, kind: crate::telemetry::incident::IncidentKind)
        -> auth::owner::IncidentOffer {
        let mut context = crate::auth::synthetic_incident();
        context.kind = kind;
        auth::owner::IncidentOffer {
            id: 7,
            key: auth::owner::IncidentKey {
                flow: auth::owner::IncidentFlow::SignIn,
                kind,
                link: context.link,
            },
            context: Some(context),
            state,
        }
    }

    fn tick_ev(ms: u32) -> ScreenEvent<SessionHost> {
        ScreenEvent::Tick(Tick { ms, dt_us: 16_667 })
    }

    fn enters_group(effects: &[Stamped<SessionHost>], group: GroupId) -> bool {
        effects.iter().any(|st| matches!(&st.fx,
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::ContainerGroup(g),
            }))) if *g == group))
    }

    /// (b) A sign-in failure whose incident has not been resolved yet is resolved by the screen
    /// that shows it, against the permission and revision it reads now — once, not every frame.
    #[test]
    fn a_pending_incident_is_resolved_once_by_the_screen_that_shows_it() {
        let _serial = crate::testlock::serial();
        let mut failed = snapshot(Phase::Error, 0, "");
        failed.error = Arc::from("Can’t reach Plex");
        failed.incident = Some(incident(auth::owner::IncidentState::Pending,
            crate::telemetry::incident::IncidentKind::PinCreate));
        let mut s = LoginScreen::new(EntryId(0), failed.read());
        let m = crate::ui::fixture::FixtureMeasure;
        let (_, first) = step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
        let resolves = |fx: &[Stamped<SessionHost>]| fx.iter().filter(|st| matches!(st.fx,
            Fx::App(AppFx::Session(auth::SessionCmd::ResolveIncident { id: 7, .. })))).count();
        assert_eq!(resolves(&first), 1, "the pending offer is resolved on the first frame it is shown");
        let (_, second) = step_ev_with(&mut s, &tick_ev(32), &failed, InstanceId(0), &m);
        assert_eq!(resolves(&second), 0, "…and not again while the owner has yet to answer");
    }

    /// (b) An offered incident puts the question on screen, with focus on its answers.
    #[test]
    fn an_offered_incident_opens_the_report_alert_with_focus_on_its_answers() {
        let _serial = crate::testlock::serial();
        let mut failed = snapshot(Phase::Error, 0, "");
        failed.incident = Some(incident(
            auth::owner::IncidentState::Offered { revision: crate::telemetry::consent::revision() },
            crate::telemetry::incident::IncidentKind::PinCreate));
        let mut s = LoginScreen::new(EntryId(0), failed.read());
        let m = crate::ui::fixture::FixtureMeasure;
        let (_, fx) = step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
        assert!(enters_group(&fx, GroupId(1)), "focus is sent to the alert's answers");
        let (_, again) = step_ev_with(&mut s, &tick_ev(32), &failed, InstanceId(0), &m);
        assert!(!enters_group(&again, GroupId(1)), "the offer is asked once");
    }

    /// Build a cx whose engine focus is on `elem`, for a `PressCommit` the trap reads it from.
    fn step_focused(
        s: &mut LoginScreen,
        ev: &ScreenEvent<SessionHost>,
        snapshot: &auth::owner::SessionSnapshot,
        elem: u32,
    ) -> Vec<Stamped<SessionHost>> {
        let m = crate::ui::fixture::FixtureMeasure;
        let mut cx = cx_with(&m, snapshot);
        cx.focus.current = Some(crate::ui::machine::FocusKey { entry: EntryId(0), elem });
        let mut present = Present::new();
        let mut buf: Vec<Stamped<SessionHost>> = Vec::new();
        {
            let mut fx = Effects::new(&mut buf, MachineId::Instance(InstanceId(0)), &mut present);
            Machine::<SessionHost>::step(s, ev, &cx, &mut fx);
        }
        buf
    }

    fn sends(fx: &[Stamped<SessionHost>]) -> bool {
        fx.iter().any(|st| matches!(st.fx, Fx::App(AppFx::Session(auth::SessionCmd::ReportIncident { id: 7 }))))
    }

    fn failed_with(state: auth::owner::IncidentState) -> auth::owner::SessionSnapshot {
        let mut failed = snapshot(Phase::Error, 0, "");
        failed.error = Arc::from("Can’t reach Plex");
        failed.incident = Some(incident(state, crate::telemetry::incident::IncidentKind::PinCreate));
        failed
    }

    /// **Details opens the card** — the one alert, as the Details sheet, with focus sent to its
    /// answers — and the read-out itself gains nothing: the row stays *Try again* / *Details*.
    #[test]
    fn details_opens_the_details_card() {
        let _serial = crate::testlock::serial();
        let failed = failed_with(auth::owner::IncidentState::NotNow);
        let mut s = LoginScreen::new(EntryId(0), failed.read());
        let m = crate::ui::fixture::FixtureMeasure;
        let (_, fx) = step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
        assert!(!enters_group(&fx, ALERT_GROUP), "an answered offer is not asked again");
        let (_, fx) = step_ev_with(&mut s, &ScreenEvent::Activate(DETAILS), &failed, InstanceId(0), &m);
        assert!(s.report.alert.is_open() && s.report.sheet == Sheet::Details);
        assert!(s.state.report.details_open);
        assert!(enters_group(&fx, ALERT_GROUP), "focus goes to the card's answers");
        assert_eq!(s.row().as_slice(), [CONTROL, DETAILS]);
    }

    #[test]
    fn open_details_card_reconciles_delivery_failure_without_reopening() {
        use auth::owner::IncidentState as S;
        use crate::ui::decision_alert::Answers;
        let _serial = crate::testlock::serial();
        let mut failed = failed_with(S::Queued { receipt: "old-receipt".into() });
        let mut s = LoginScreen::new(EntryId(0), failed.read());
        let m = crate::ui::fixture::FixtureMeasure;
        step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
        step_ev_with(&mut s, &ScreenEvent::Activate(DETAILS), &failed, InstanceId(0), &m);
        assert_eq!(s.report.alert.answers(), Answers::One);
        assert!(s.report.alert.body_for_test()[0].starts_with("Report ID:"));
        failed.incident.as_mut().unwrap().state = S::Failed;
        step_ev_with(&mut s, &tick_ev(32), &failed, InstanceId(0), &m);
        assert!(s.report.alert.is_open());
        assert_eq!(s.report.alert.answers(), Answers::Two, "a failed delivery can be sent again while Details stays open");
        assert_eq!(s.report.alert.body_for_test(), s.report.details_body(), "the retired receipt must leave the card");
        assert_eq!(s.report.alert.choice(), Choice::Cancel, "adding Send must not steal focus from Close");
    }

    #[test]
    fn open_details_card_reconciles_a_receipt_arriving_while_open() {
        use auth::owner::IncidentState as S;
        let _serial = crate::testlock::serial();
        let mut failed = failed_with(S::Sending);
        let mut s = LoginScreen::new(EntryId(0), failed.read());
        let m = crate::ui::fixture::FixtureMeasure;
        step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
        step_ev_with(&mut s, &ScreenEvent::Activate(DETAILS), &failed, InstanceId(0), &m);
        assert!(!s.report.alert.body_for_test().iter().any(|p| p.starts_with("Report ID:")));
        failed.incident.as_mut().unwrap().state = S::Queued { receipt: "0123456789abcdef".into() };
        step_ev_with(&mut s, &tick_ev(32), &failed, InstanceId(0), &m);
        assert!(s.report.alert.is_open());
        assert_eq!(s.report.alert.body_for_test()[0], "Report ID: 0123 4567 89ab cdef",
            "the same incident gained a receipt after the card opened");
        assert_eq!(s.report.alert.body_for_test(), s.report.details_body());
        // A new value within the SAME state variant must repaint too: the card renders the
        // receipt, not merely the incident's ID or the Queued discriminator.
        failed.incident.as_mut().unwrap().state = S::Queued { receipt: "fedcba9876543210".into() };
        step_ev_with(&mut s, &tick_ev(48), &failed, InstanceId(0), &m);
        assert_eq!(s.report.alert.body_for_test()[0], "Report ID: fedc ba98 7654 3210");
    }

    #[test]
    fn open_details_card_reconciles_focus_when_send_is_removed() {
        use auth::owner::IncidentState as S;
        let _serial = crate::testlock::serial();
        let mut failed = failed_with(S::NotNow);
        let mut s = LoginScreen::new(EntryId(0), failed.read());
        let m = crate::ui::fixture::FixtureMeasure;
        step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
        step_ev_with(&mut s, &ScreenEvent::Activate(DETAILS), &failed, InstanceId(0), &m);
        assert_eq!(s.report.alert.choice(), Choice::Destructive);
        failed.incident.as_mut().unwrap().state = S::Queued { receipt: "receipt".into() };
        step_focused(&mut s, &tick_ev(32), &failed, ALERT_SEND);
        assert_eq!(s.report.alert.choice(), Choice::Cancel);
        let cx = cx_with(&m, &failed);
        let key = <LoginScreen as Focusable<SessionHost>>::reconcile(&s, s.key(ALERT_SEND), &cx);
        assert_eq!(key, s.key(ALERT_CANCEL));
        assert!(<LoginScreen as Focusable<SessionHost>>::place(&s, &key.elem, &cx, At::Drawn).is_some());
    }

    /// **Send report is on the card iff the report can still be sent**, and holds focus when it is
    /// there; otherwise *Close* is the one answer. The body carries the Report ID once there is one,
    /// then the support line.
    #[test]
    fn the_details_card_offers_send_report_iff_sendable() {
        use auth::owner::IncidentState as S;
        use crate::ui::decision_alert::Answers;
        let _serial = crate::testlock::serial();
        let m = crate::ui::fixture::FixtureMeasure;
        let cx = test_cx(&m);
        let pin = crate::telemetry::incident::IncidentKind::PinCreate;
        let receipt = "41de4cd388e4041654de38f2787c3922".to_string();
        for state in [S::NotNow, S::Failed, S::OnRequest { revision: 0 }, S::Sending,
            S::Delivered { receipt: receipt.clone() }, S::Saved { receipt: receipt.clone() }]
        {
            let mut s = screen_with(Phase::Error, state.clone(), pin);
            let sendable = s.report.sendable();
            let mut buf: Vec<Stamped<SessionHost>> = Vec::new();
            let mut present = Present::new();
            {
                let mut fx = Effects::new(&mut buf, MachineId::Instance(InstanceId(0)), &mut present);
                s.activate_elem(DETAILS, &mut fx);
            }
            let want = if sendable { Answers::Two } else { Answers::One };
            assert_eq!(s.report.alert.answers(), want, "{state:?}");
            assert_eq!(s.alert_elems().contains(&ALERT_SEND), sendable, "{state:?}");
            let seat = <LoginScreen as Focusable<SessionHost>>::seat(
                &s, ALERT_GROUP, Placed { rect: Rect::FULL, rest_rect: Rect::FULL, clip: Rect::FULL, index: None }, &cx);
            assert_eq!(seat.elem, if sendable { ALERT_SEND } else { ALERT_CANCEL }, "{state:?}");
            let body = s.report.details_body();
            let support = "PlxNative 0 · webOS 0 · set · pin_create";
            if matches!(state, S::Delivered { .. } | S::Saved { .. }) {
                assert_eq!(body, ["Report ID: 41de 4cd3 88e4 0416 54de 38f2 787c 3922", support], "{state:?}");
            } else {
                assert_eq!(body, [support], "{state:?}");
            }
            s.report.alert.close();
        }
        // The two ends of the sendable question, stated rather than derived.
        assert!(screen_with(Phase::Error, S::NotNow, pin).report.sendable());
        assert!(!screen_with(Phase::Error, S::Delivered { receipt }, pin).report.sendable());
    }

    /// **BACK and Close close the card and put focus back on Details** — and neither sends
    /// anything or leaves the screen; with the card closed, BACK is the root press it always was.
    #[test]
    fn back_or_close_returns_focus_to_details() {
        let _serial = crate::testlock::serial();
        let failed = failed_with(auth::owner::IncidentState::NotNow);
        let m = crate::ui::fixture::FixtureMeasure;
        for via_back in [true, false] {
            let mut s = LoginScreen::new(EntryId(0), failed.read());
            step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
            step_ev_with(&mut s, &ScreenEvent::Activate(DETAILS), &failed, InstanceId(0), &m);
            assert!(s.report.alert.is_open());
            let fx = if via_back {
                step_ev_with(&mut s, &key_back_down(), &failed, InstanceId(0), &m).1
            } else {
                step_focused(&mut s, &ScreenEvent::PressCommit(crate::ui::machine::PressId(1)), &failed, ALERT_CANCEL)
            };
            assert!(!s.report.alert.is_open(), "via_back={via_back}: the card closes");
            assert!(enters_elem(&fx, DETAILS), "via_back={via_back}: focus returns to Details");
            assert!(!sends(&fx), "via_back={via_back}: nothing is sent");
            assert_eq!(root_backs(&fx), 0, "via_back={via_back}: the screen stays");
        }
        let mut s = LoginScreen::new(EntryId(0), failed.read());
        step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
        let (_, fx) = step_ev_with(&mut s, &key_back_down(), &failed, InstanceId(0), &m);
        assert_eq!(root_backs(&fx), 1, "with the card closed, BACK is the root press");
    }

    /// **Send report on the card sends the held report**, closes the card and returns focus to
    /// Details, where the status line then reports it.
    #[test]
    fn send_report_on_the_card_sends_the_report() {
        let _serial = crate::testlock::serial();
        let failed = failed_with(auth::owner::IncidentState::NotNow);
        let m = crate::ui::fixture::FixtureMeasure;
        let mut s = LoginScreen::new(EntryId(0), failed.read());
        step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
        let (_, early) = step_ev_with(&mut s, &ScreenEvent::Activate(ALERT_SEND), &failed, InstanceId(0), &m);
        assert!(!sends(&early), "no answer is live before the card is open");
        step_ev_with(&mut s, &ScreenEvent::Activate(DETAILS), &failed, InstanceId(0), &m);
        let fx = step_focused(&mut s, &ScreenEvent::PressCommit(crate::ui::machine::PressId(1)), &failed, ALERT_SEND);
        assert!(sends(&fx), "Send report sends the held offer");
        assert!(!s.report.alert.is_open(), "…and closes the card");
        assert!(enters_elem(&fx, DETAILS), "…handing focus back to Details");
    }

    // ---- PLX-NATIVE-10: the consent question ----

    fn plaintext_verdict(eligibility: crate::plex::probe::PlaintextEligibility)
        -> auth::PlaintextVerdict {
        auth::PlaintextVerdict {
            machine_id: "lan-machine".into(),
            name: "Home".into(),
            shared_by: String::new(),
            eligibility,
            choice: crate::plex::session::PlaintextChoice::Undecided,
        }
    }

    /// An offered report on an insecure-only failure, with `eligibility`'s verdict.
    fn insecure_failure(eligibility: crate::plex::probe::PlaintextEligibility)
        -> auth::owner::SessionSnapshot {
        let mut failed = failed_with(auth::owner::IncidentState::Offered {
            revision: crate::telemetry::consent::revision(),
        });
        let verdict = plaintext_verdict(eligibility);
        failed.error = Arc::from(auth::insecure_only_copy(Some(&verdict)).as_ref());
        failed.plaintext = Some(verdict);
        failed
    }

    fn answers(effects: &[Stamped<SessionHost>]) -> Vec<bool> {
        effects.iter().filter_map(|st| match &st.fx {
            Fx::App(AppFx::Session(auth::SessionCmd::AnswerPlaintext { machine_id, choice, sid: None }))
                if machine_id == "lan-machine" => Some(*choice == crate::plex::session::PlaintextChoice::Allowed),
            _ => None,
        }).collect()
    }

    /// **An eligible plaintext-only server is a choice, not a failure**: the read-out's one primary
    /// is *Connect* beside *Details* — never a third button — and the report question is NOT raised
    /// on its own; the report stays behind *Details*.
    #[test]
    fn an_eligible_plaintext_failure_offers_connect_and_does_not_raise_the_report_question() {
        let _serial = crate::testlock::serial();
        let failed = insecure_failure(crate::plex::probe::PlaintextEligibility::Eligible);
        let mut s = LoginScreen::new(EntryId(0), failed.read());
        let m = crate::ui::fixture::FixtureMeasure;
        let (_, fx) = step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
        assert!(!enters_group(&fx, ALERT_GROUP), "the report question is not raised");
        assert!(!s.report.alert.is_open());
        assert_eq!(s.control_kind(), Some(ControlKind::ConnectPlaintext));
        assert_eq!(s.readout_labels().0, [Some(CONNECT), Some(DETAILS_LABEL)]);
        assert_eq!(s.row().as_slice(), [CONTROL, DETAILS]);
        assert_eq!(s.state.plaintext, Some(false));
        assert!(answers(&fx).is_empty(), "nothing is answered for the person");
    }

    /// **The DRAWN primary is the one the press acts on** (design review D1). The failed read-out
    /// painted *Try again* over a primary that asked the question — the geometry and the press
    /// read `control_kind`, the paint a hard-coded label. Every stage is checked on the overlay
    /// the draw paints (`failed_readout`), not on `readout_labels`: an unanswered eligible server
    /// draws *Connect*; once answered (*Not now*) it draws *Try again*, which is what it does.
    #[test]
    fn the_failed_readout_draws_the_label_its_press_acts_on() {
        use crate::plex::session::PlaintextChoice;
        let _serial = crate::testlock::serial();
        let m = crate::ui::fixture::FixtureMeasure;
        let mut failed = insecure_failure(crate::plex::probe::PlaintextEligibility::Eligible);
        for (choice, want) in [(PlaintextChoice::Undecided, CONNECT), (PlaintextChoice::Declined, ESCAPE),
            (PlaintextChoice::Revoked, ESCAPE), (PlaintextChoice::Allowed, ESCAPE)] {
            if let Some(v) = failed.plaintext.as_mut() {
                v.choice = choice;
            }
            let mut s = LoginScreen::new(EntryId(0), failed.read());
            step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
            let reason = CString::new(s.error.as_ref()).unwrap();
            let drawn = s.failed_readout(&reason, None);
            assert_eq!(drawn.action, Some(want), "{choice:?}: the drawn primary");
            assert_eq!(drawn.action, s.control_kind().map(label_for), "{choice:?}: drawn = pressed");
        }
    }

    /// **Connect asks, and only the answer reaches Session.** The question opens seated on
    /// *Not now*; *Connect* allows, *Not now* and BACK decline — each exactly once, and focus comes
    /// back to the read-out.
    #[test]
    fn connect_asks_the_question_and_only_its_answer_reaches_session() {
        let _serial = crate::testlock::serial();
        let failed = insecure_failure(crate::plex::probe::PlaintextEligibility::Eligible);
        let m = crate::ui::fixture::FixtureMeasure;
        for (how, allow) in [("connect", true), ("not now", false), ("back", false)] {
            let mut s = LoginScreen::new(EntryId(0), failed.read());
            step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
            let (_, opened) = step_ev_with(&mut s, &ScreenEvent::Activate(CONTROL), &failed, InstanceId(0), &m);
            assert!(answers(&opened).is_empty(), "{how}: opening the question answers nothing");
            assert!(s.report.alert.is_open() && s.report.sheet == Sheet::Plaintext, "{how}");
            assert!(enters_group(&opened, ALERT_GROUP), "{how}");
            assert_eq!(s.state.plaintext, Some(true));
            let cx_m = crate::ui::fixture::FixtureMeasure;
            let cx = cx_with(&cx_m, &failed);
            assert_eq!(Focusable::<SessionHost>::seat(&s, ALERT_GROUP, Placed { rect: Rect::FULL, rest_rect: Rect::FULL, clip: Rect::FULL, index: None }, &cx).elem,
                ALERT_CANCEL, "{how}: seated on Not now");
            let fx = match how {
                "connect" => step_focused(&mut s, &ScreenEvent::PressCommit(crate::ui::machine::PressId(1)), &failed, ALERT_SEND),
                "not now" => step_focused(&mut s, &ScreenEvent::PressCommit(crate::ui::machine::PressId(1)), &failed, ALERT_CANCEL),
                _ => step_ev_with(&mut s, &key_back_down(), &failed, InstanceId(0), &m).1,
            };
            assert_eq!(answers(&fx), [allow], "{how}");
            assert!(!sends(&fx), "{how}: no report is sent");
            assert_eq!(root_backs(&fx), 0, "{how}: the screen stays");
            assert!(!s.report.alert.is_open(), "{how}");
            assert!(enters_group(&fx, CONTROL_GROUP), "{how}: focus returns to the read-out");
        }
    }

    /// A verdict that cannot be offered (remote-only here) keeps today's read-out: *Try again*, and
    /// the report question raised as for any failure.
    #[test]
    fn an_ineligible_plaintext_failure_keeps_try_again_and_the_report_question() {
        let _serial = crate::testlock::serial();
        let failed = insecure_failure(crate::plex::probe::PlaintextEligibility::NotLocal);
        let mut s = LoginScreen::new(EntryId(0), failed.read());
        let m = crate::ui::fixture::FixtureMeasure;
        let (_, fx) = step_ev_with(&mut s, &tick_ev(16), &failed, InstanceId(0), &m);
        assert_eq!(s.control_kind(), Some(ControlKind::Retry));
        assert!(enters_group(&fx, ALERT_GROUP), "the report question is asked");
        assert_eq!(s.report.sheet, Sheet::Question);
        assert_eq!(s.state.plaintext, None);
    }

    fn root_backs(effects: &[Stamped<SessionHost>]) -> usize {
        effects
            .iter()
            .filter(|st| matches!(st.fx, Fx::App(AppFx::Session(auth::SessionCmd::BackAtRoot { .. }))))
            .count()
    }

    fn enters_elem(effects: &[Stamped<SessionHost>], elem: u32) -> bool {
        effects.iter().any(|st| matches!(&st.fx,
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::Elem(k),
            }))) if k.elem == elem))
    }

    /// **(e) One quiet line says what became of the report**, and only once there is something to
    /// say. Queued is not sent: "sent" is kept for a server's acceptance. The line never carries
    /// the Report ID — that lives in Details.
    #[test]
    fn the_report_status_is_one_short_line_without_the_report_id() {
        use auth::owner::IncidentState as S;
        let r = || "0123abcd".to_string();
        let line = |state: S| report_status(&state).map(|(t, busy)| (t.to_str().unwrap().to_string(), busy));
        let expect = |text: &str, busy: bool| Some((text.to_string(), busy));
        assert_eq!(line(S::Sending), expect("Sending report\u{2026}", true));
        assert_eq!(line(S::AutoSending), expect("Sending report\u{2026}", true));
        assert_eq!(line(S::Queued { receipt: r() }), expect("Sending report\u{2026}", true));
        assert_eq!(line(S::Saved { receipt: r() }), expect("Report saved. It will be sent later.", false));
        assert_eq!(line(S::Delivered { receipt: r() }), expect("Report sent. Thank you.", false));
        assert_eq!(line(S::Failed), expect("Report couldn\u{2019}t be sent.", false));
        for quiet in [S::Pending, S::NotNow, S::Dropped, S::Offered { revision: 0 }, S::OnRequest { revision: 0 }] {
            assert_eq!(line(quiet.clone()), None, "{quiet:?}");
        }
    }

    /// The Report ID reads aloud in groups of four, lowercase, whatever separators it came with.
    #[test]
    fn the_report_id_is_grouped_in_fours() {
        assert_eq!(
            group_report_id("41de4cd388e4041654de38f2787c3922"),
            "41de 4cd3 88e4 0416 54de 38f2 787c 3922"
        );
        assert_eq!(
            group_report_id("41DE4CD3-88E4-0416-54DE-38F2787C3922"),
            "41de 4cd3 88e4 0416 54de 38f2 787c 3922"
        );
        assert_eq!(report_id_line("0123abcd"), "Report ID: 0123 abcd");
    }

    /// **The Details card's support line names the same storage evidence `event_body` would
    /// send** — one source, two projections (spec: `telemetry::incident::storage_evidence_line`).
    /// An offer with no persistence/key-manager/service evidence still shows the fixed `unknown`
    /// triple rather than dropping the segment, and an offer with none at all (a Declined incident
    /// keeps no context) reads identically.
    #[test]
    fn support_line_carries_the_same_storage_evidence_event_body_would_send() {
        let plain = incident(
            auth::owner::IncidentState::NotNow,
            crate::telemetry::incident::IncidentKind::PinCreate,
        );
        assert!(
            support_line(&plain).ends_with("persistence:unknown keymgr:unknown svc:unknown"),
            "{}",
            support_line(&plain)
        );

        let mut with_evidence = plain.clone();
        let mut ctx = crate::auth::synthetic_incident();
        ctx.kind = crate::telemetry::incident::IncidentKind::SaveFailed;
        ctx.persistence = Some(crate::telemetry::incident::PersistenceFailure::WriteFailed);
        ctx.keymanager_stage = Some(crate::storage::wire::KeymanagerStage::Begin);
        ctx.service_error_code = Some(-17);
        with_evidence.key.kind = ctx.kind;
        with_evidence.context = Some(ctx);
        assert!(
            support_line(&with_evidence)
                .ends_with("persistence:write_failed keymgr:begin svc:-17"),
            "{}",
            support_line(&with_evidence)
        );

        let mut declined = plain.clone();
        declined.context = None;
        assert!(
            support_line(&declined).ends_with("persistence:unknown keymgr:unknown svc:unknown"),
            "a declined offer keeps no context, and the line still reads unknown, not blank"
        );
    }

    fn screen_with(phase: Phase, state: auth::owner::IncidentState,
        kind: crate::telemetry::incident::IncidentKind) -> LoginScreen {
        let mut s = bare_screen(phase, 0.0);
        s.error = Arc::from("Can’t reach Plex");
        s.report.offer = Some(incident(state, kind));
        s.report.support = CString::new("PlxNative 0 · webOS 0 · set · pin_create").unwrap();
        s
    }

    fn text(note: Option<Note>) -> Option<String> {
        note.map(|n| n.text.to_str().unwrap().to_string())
    }

    /// **The calm default**: a failure nobody has reported draws its verdict, reason and row and
    /// nothing else — no status line — and a report's line is its status alone: the Report ID
    /// and the support line are the Details card's, never the read-out's.
    #[test]
    fn a_failure_with_no_report_draws_no_status_line() {
        use auth::owner::IncidentState as S;
        let pin = crate::telemetry::incident::IncidentKind::PinCreate;
        for state in [S::Pending, S::Offered { revision: 0 }, S::NotNow, S::Dropped, S::OnRequest { revision: 0 }] {
            let s = screen_with(Phase::Error, state.clone(), pin);
            assert_eq!(text(s.report_note()), None, "{state:?}");
        }
        let delivered = S::Delivered { receipt: "41de4cd388e4041654de38f2787c3922".into() };
        let s = screen_with(Phase::Error, delivered, pin);
        assert_eq!(text(s.report_note()).as_deref(), Some("Report sent. Thank you."));
    }

    /// **(e) A report on its way keeps the inline spinner turning over a settled read-out** — the
    /// failed sign-in draws no spinner of its own, so its clock must still run for the report's,
    /// and stop once the report has settled.
    #[test]
    fn a_report_on_its_way_turns_the_spinner_on_a_failed_readout() {
        let _serial = crate::testlock::serial();
        let m = crate::ui::fixture::FixtureMeasure;
        let spin_after_ticks = |state: auth::owner::IncidentState| {
            let mut failed = snapshot(Phase::Error, 0, "");
            failed.incident = Some(incident(state, crate::telemetry::incident::IncidentKind::PinCreate));
            let mut s = LoginScreen::new(EntryId(0), failed.read());
            for ms in [16, 32, 48] {
                step_ev_with(&mut s, &tick_ev(ms), &failed, InstanceId(0), &m);
            }
            s.spin_ms
        };
        assert!(spin_after_ticks(auth::owner::IncidentState::Queued { receipt: "r".into() }) > 0.0, "queued: turning");
        assert_eq!(spin_after_ticks(auth::owner::IncidentState::Delivered { receipt: "r".into() }), 0.0, "delivered: still");
    }

}
