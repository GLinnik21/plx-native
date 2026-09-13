//! The sign-in screen: Plex's own server-rendered QR PNG (fetched by the auth flow, decoded +
//! tinted here) plus the typed short-code fallback, driven by the [`crate::auth`] flow phase.
//! Scanning the QR on a phone opens plex.tv pre-filled with the pin; the flow's background poll
//! then advances us onward.
#![allow(non_upper_case_globals)]
use crate::auth::{self, Phase};
use crate::ui::consts::*;
use crate::ui::decision_alert::{Choice, DecisionAlert, Tone};
use crate::ui::label::{HAlign, Label};
use crate::ui::popover::Popover;
use crate::ui::route_screen::{ActionRow, RouteGround, RouteLayout};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::text_view::TextView;
use crate::ui::widgets::{Button, Spinner, StatusKind, StatusOverlay};
use crate::ui::{theme, Env, Painter, Rect, View};
use crate::telemetry::oneoff::{self, DeliveryState};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::addr_of_mut;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Scene {
    spin_ms: f32,
    qr_tex: u32, // GL texture of Plex's QR PNG (0 until decoded+uploaded)
    /// Which code `qr_tex` holds ([`auth::qr_generation`]). The cache key, and the reason a code
    /// replaced mid-`Waiting` cannot be drawn after it has died.
    qr_gen: u64,
    ground: RouteGround,
    /// How long the CURRENT wait has been on screen. Distinct from `spin_ms`, which is a
    /// free-running rotation clock: this one is reset by [`update`] whenever the wait changes,
    /// because the only question it answers is "has this particular wait gone on too long".
    phase_ms: f32,
    /// What that wait IS — see [`wait_id`]. A phase alone was not enough once a code could be
    /// replaced without leaving [`Phase::Waiting`].
    wait: (Phase, u64),
    /// How many files the last **Delete all local data** could not unlink. Drives the wording of
    /// the `Deleted` read-out, which must not claim a wipe it did not achieve.
    delete_leftovers: usize,
    /// The link-health sentence ([`auth::link_detail`]) this screen last sampled, or `None` while
    /// plex.tv was answering. Cached so [`update`] can tell whether the underlying state actually
    /// changed — the link snapshot is sampled every frame, but the sentence itself only moves
    /// changes roughly once a poll (~2 s), without waking every frame for an equal value.
    last_link_detail: Option<String>,
    /// A link-detail repaint deferred while the report alert freezes the QR page beneath it.
    link_detail_repaint_pending: bool,
    /// **Issue #75** — the one-off sign-in report alert, opened over the failed or stuck read-out.
    report_alert: DecisionAlert,
    /// Which attempt [`report_alert`] was last opened (or answered) for — see [`offer_alert`].
    /// `None` before this boot has offered one at all. Never explicitly reset on a flow restart:
    /// `auth::trouble_snapshot`'s attempt id keeps climbing, so a fresh attempt's id can never
    /// equal an old one this field remembers, and the alert opens again on its own.
    report_offered_for: Option<u64>,
    /// **Issue #75 review.** Did the most recent deliberate "Send report" press fail to queue
    /// (no Sentry endpoint, no `/dev/urandom`, or the durable spool refused it)? Reset to `false`
    /// every time [`report_alert`] opens for a new attempt, so a stale failure from a previous
    /// trouble cannot linger over one this attempt never tried to send. Drives
    /// [`report_note`]'s third state — without it a failed press was completely silent, dismissing
    /// the alert and leaving the read-out byte-identical to "Not now".
    report_send_failed: bool,
    /// **Issue #76's report lane.** Everything the "Details" panel over the storage read-out can
    /// show, as the integrator last told this screen via [`note_storage_readout`]. Never reset by
    /// [`enter`] — the same reasoning [`delete_leftovers`](Self::delete_leftovers) is not: it is
    /// set once the integrator knows it, not per visit.
    storage_readout: StorageReadout,
    /// The current one-off receipt belongs to this attempt only. It is intentionally not
    /// persisted or reused after a route reset/sign-out.
    storage_report_attempt: Option<u64>,
    storage_report_id: Option<String>,
    storage_report_state: Option<DeliveryState>,
    storage_report_failed: bool,
    report_prompt_storage_attempt: Option<u64>,
    report_prompt_warning: Option<auth::PersistenceWarningKey>,
    persistence_warning_seen: Option<auth::PersistenceWarningKey>,
    details_report_rect: Option<Rect>,
}

/// How long a working phase runs before the read-out grows a way out.
///
/// **Not zero**: a healthy LAN discovery finishes in well under a second, and a control that
/// flashes past on every sign-in is noise that teaches people to ignore it. **Not longer**: from
/// the sofa a spinner that will never stop looks exactly like one that is about to, and until this
/// existed there was no way at all out of a wedged sign-in — BACK is swallowed on a first-ever
/// boot (`auth::cancel` has no stored session to resume), so the only exit was killing the app.
const ESCAPE_AFTER_MS: f32 = 12_000.0;

/// Whether a wait has run long enough to be worth offering an escape from.
///
/// Pure and separate from the draw so the threshold is gradeable on the host; the state it reads
/// lives in the SDL loop's own scene.
fn escape_offered(phase_ms: f32) -> bool {
    phase_ms >= ESCAPE_AFTER_MS
}

/// **The sealed-sign-in-could-not-be-read read-out (issue #76 review).**
///
/// It names the SUBJECT (the sign-in saved on this television), the CAUSE in the words a person
/// can act on (the television's own key service did not answer — not "keymanager3", not an error
/// code; the footer already carries the build and the set for a photograph), and the fact that
/// matters most and is least guessable: **nothing has been changed**. The stored sign-in is still
/// there, byte for byte, and the next launch — or the button under this copy — may well open it.
///
/// The two ways out are stated in the order they should be tried, and the second one is the QR
/// stack already drawn beside this column, which is why the copy points at it rather than the
/// screen growing a second control for it. "Only signing in again replaces it" is the promise the
/// storage layer actually keeps (`plex::session::seal_permitted`'s own doc), and saying it here is
/// what stops *Try again* reading like a gamble with the stored credential.
const STORAGE_TITLE: &str = "Your saved sign-in couldn\u{2019}t be read";
const STORAGE_COPY: &str = "This television\u{2019}s key service didn\u{2019}t answer, so the \
sign-in saved here couldn\u{2019}t be unlocked. Nothing has been changed \u{2014} try again, or \
scan the code to sign in again. Only signing in again replaces what is stored here.";

/// The label on the storage read-out's one control. Deliberately the same words as [`ESCAPE`] —
/// it is the same offer (ask the thing that did not answer, again) about a different subject, and
/// two different verbs for one gesture is how a screen teaches somebody that it is unpredictable.
const STORAGE_RETRY: &std::ffi::CStr = c"Try again";

/// What the *Try again* press is doing right now — see [`start_storage_retry`].
///
/// An atomic rather than a [`Scene`] field because the ask itself runs on a WORKER: reopening the
/// envelope is one or two LS2 calls against a service that has already proved it can go quiet, and
/// `keymanager::platform::BUDGET` is measured in seconds. Doing that inline would freeze the SDL
/// main loop — the spinner, the QR, every key — for the whole budget, on the one screen whose
/// subject is a television that seems stuck.
static STORAGE_RETRY_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(RETRY_IDLE);
const RETRY_IDLE: u8 = 0;
/// A worker is asking the key service right now.
const RETRY_BUSY: u8 = 1;
/// The worker opened the envelope. The MAIN thread picks this up in [`update`] — resuming the
/// session installs a server registry and a profile, which is main-thread work.
const RETRY_OPENED: u8 = 2;
/// The worker finished and the envelope is still not open.
const RETRY_FAILED: u8 = 3;

/// **What [`update`] last saw [`STORAGE_RETRY_STATE`] at** — the main thread's own edge detector,
/// so the Details panel can be re-read the moment a *Try again* press settles.
///
/// The press changes exactly the facts that panel shows (the storage class, the key service's last
/// refusal, and — on a set that grants the app id on the second registration — the identity), and
/// it changes them from a WORKER, which may not touch `Scene`. So the worker publishes a state and
/// this notices the edge one frame later on the thread that owns the screen. Only settled states
/// trigger the re-read: the transition INTO `RETRY_BUSY` has learned nothing yet.
static STORAGE_RETRY_SEEN: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(RETRY_IDLE);

/// **Which sign-in attempt [`start_storage_retry`] was pressed for — [`settle_storage_retry`]'s
/// stale-result guard.** `0` is never a real attempt (`auth`'s counter starts at 1), so it also
/// means "nothing captured yet".
///
/// The press and the worker it starts are asking about the flow's CURRENT attempt at that
/// instant, not about whatever the flow happens to be doing several hundred milliseconds later
/// when the worker returns — the whole gap this screen's own async note (`storage_note`) exists
/// to narrate. If, in that gap, the SAME running attempt reaches its own `Phase::Ready` (a QR scan
/// completed on the phone while the television was still asking its key service about a
/// completely different, older account), the attempt id alone cannot see it — a normal completion
/// never bumps `Ctl::attempt`, only a fresh reset does. [`settle_storage_retry`] therefore checks
/// the PHASE this was captured for as well: it only ever fires while `auth::phase()` is still
/// [`Phase::Waiting`], the one phase the storage read-out draws over, so a flow that has already
/// moved on — Ready, Profiles, Error, a fresh attempt, anything — is left alone and the newer
/// result wins.
static STORAGE_RETRY_ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// ---- Issue #76's report lane: the "Details" panel over the storage read-out ----------------------
//
// Owner correction, 2026-09-11: no diagnostic words in the read-out line or the footer — instead a
// second pill, "Details", beside "Try again", opening an in-screen panel on the app's existing menu
// idiom (`ui::CLAUDE.md`: Popover + TableView, tokens and components only). One row per fact, label
// left / value right, photographable. BACK closes it.

/// One candidate session-file location's read outcome, in the read lane's own closed vocabulary —
/// see `telemetry::storage::StorageExtra::candidate_reads`'s wire shape for where these words come
/// from. `outcome` is either a rejection word (`missing`/`open_failed`/`not_regular`/`wrong_owner`/
/// `metadata_failed`/`too_large`/`read_failed`/`untrusted_mode`/`unparsable`) or, for a candidate that DID read,
/// the storage-class word it produced (`plaintext`/`secure`/…).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateReadout {
    pub category: &'static str,
    pub outcome: &'static str,
    pub errno: Option<i32>,
}

/// Everything the "Details" panel can show, as the integrator last told this screen. Every field is
/// `None`/empty until [`note_storage_readout`] is called at least once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct StorageReadout {
    /// "Stored sign-in" — the storage class word (`plex::session::storage_class`'s own vocabulary).
    stored_sign_in: Option<&'static str>,
    /// "Last save" — the save lane's persist-outcome word (`telemetry::storage::StorageExtra`'s own
    /// vocabulary).
    persist_outcome: Option<&'static str>,
    /// Only meaningful alongside `persist_outcome`: WHY, when that word is
    /// `preserved_existing_secure`.
    preserve_reason: Option<&'static str>,
    /// "Key service" — the STAGE word of the last refusal this launch's key service gave
    /// (`keymanager::LastRefusal::stage`), i.e. the reason `plex::session::secure_unavailable()`
    /// is true. `None` on a launch that never reached a refusal.
    key_service: Option<&'static str>,
    /// "Key identity" — WHICH LS2 identity this launch registered under
    /// (`keymanager::Identity::code`: `app_id`/`named`/`anonymous`). A separate row from
    /// `key_service` because it is a separate fact and issue #76's whole hypothesis is that it
    /// differs between the launch that SEALED and the launch that cannot reopen — a photograph of
    /// this panel from two launches is the cheapest way anyone has to see that happen.
    key_identity: Option<&'static str>,
    /// One row per candidate session-file location this launch checked.
    candidates: Vec<CandidateReadout>,
    /// Cold-boot eligibility captured before client-id minting or resealing.
    cold_winner: Option<&'static str>,
    cold_account: Option<&'static str>,
    cold_pms: Option<&'static str>,
    cold_server: Option<&'static str>,
    cold_local: Option<&'static str>,
    /// Fresh-sign-in disk readback, when the persistence lane has performed one.
    fresh_readback: Option<String>,
}

impl StorageReadout {
    /// Nothing to show yet — the "Details" pill stays off the action row entirely rather than
    /// opening onto an empty panel.
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.stored_sign_in.is_none()
            && self.persist_outcome.is_none()
            && self.key_service.is_none()
            && self.key_identity.is_none()
            && self.candidates.is_empty()
    }
}

/// **The setter**: replaces whatever the screen held before — there is only ever one launch's worth
/// of facts to show, never a history of them. [`refresh_storage_readout`] is the one caller in the
/// app; this stays separate from it so the row set can be driven directly from a host test with no
/// `plex::session` or `keymanager` state to arrange.
fn note_storage_readout(
    stored_sign_in: Option<&'static str>,
    persist_outcome: Option<&'static str>,
    preserve_reason: Option<&'static str>,
    key_service: Option<&'static str>,
    key_identity: Option<&'static str>,
    candidates: Vec<CandidateReadout>,
) {
    scene().storage_readout = StorageReadout {
        stored_sign_in,
        persist_outcome,
        preserve_reason,
        key_service,
        key_identity,
        candidates,
        ..StorageReadout::default()
    };
    // A discrete change a settled screen must not miss — `ui::idle`'s rule for exactly this shape
    // of update (an async landing, not a spring).
    crate::ui::idle::invalidate();
}

/// **Read this launch's storage facts, now, and hand them to the screen** — the whole of what the
/// "Details" panel can say.
///
/// Every source is a live process-global that only moves at the few moments this is called from:
/// the boot `session::load` (`app.rs`), every entry to this route ([`enter`]), and the settling of
/// a *Try again* press ([`settle_storage_retry`], which is where they change under a screen that
/// is already up). **Never per frame** — `session::storage_class` opens up to five marker paths to
/// answer, which is the syscall storm `session::secure_unavailable`'s own doc exists to keep off
/// the SDL main thread.
pub fn refresh_storage_readout() {
    use crate::plex::session;
    let candidates = session::last_candidate_reads()
        .iter()
        .map(|c| CandidateReadout {
            category: c.category.wire(),
            // The same two-source outcome word `session::candidate_reads_wire` builds its line
            // from, and it must stay the same vocabulary: a rejection word for a candidate this
            // launch declined, the accepted shape for one it read. `unknown` is unreachable today
            // (every recorded candidate carries one or the other) and is the honest answer rather
            // than an empty cell if a third shape is ever recorded.
            outcome: match (c.rejection, c.accepted_as) {
                (Some(rej), _) => rej.wire(),
                (None, Some(accepted)) => accepted,
                (None, None) => "unknown",
            },
            errno: c.rejection.and_then(|rej| rej.errno()),
        })
        .collect();
    let persist = session::last_persist_outcome();
    note_storage_readout(
        Some(session::storage_class().code()),
        persist.map(|o| o.wire()),
        persist.and_then(|o| o.reason_wire()),
        crate::keymanager::last_refusal().map(|r| r.stage.code()),
        crate::keymanager::identity().map(|i| i.code()),
        candidates,
    );
    let s = scene();
    if let Some(facts) = session::cold_session_facts() {
        s.storage_readout.cold_winner = facts.winner.map(|w| w.wire());
        s.storage_readout.cold_account = Some(if facts.account_token_present { "yes" } else { "no" });
        s.storage_readout.cold_pms = Some(if facts.pms_token_present { "yes" } else { "no" });
        s.storage_readout.cold_server = Some(if facts.server_dialable { "yes" } else { "no" });
        s.storage_readout.cold_local = Some(if facts.can_go_local { "yes" } else { "no" });
    }
    if let Some(readback) = session::last_fresh_save_readback() {
        s.storage_readout.fresh_readback = Some(format!(
            "{} ({})",
            readback.winner.wire(),
            readback.result.wire()
        ));
    }
}

/// PURE. A human label for one candidate category word — the four the read lane names today, plus
/// a fallback so a fifth one added later still reads as *something* rather than vanishing silently.
fn candidate_category_label(category: &str) -> String {
    match category {
        "developer" => "Developer directory".to_string(),
        "internal" => "Internal storage".to_string(),
        "app_dir" => "App directory".to_string(),
        "runtime" => "Runtime directory".to_string(),
        other => format!("{other} directory"),
    }
}

/// PURE. One candidate row's value: its outcome word, with `(errno N)` appended when there is one.
fn format_candidate_value(outcome: &str, errno: Option<i32>) -> String {
    match errno {
        Some(e) => format!("{outcome} (errno {e})"),
        None => outcome.to_string(),
    }
}

/// PURE. Assemble the Details panel's rows — label left, value right — from the current readout
/// plus the footer's own "Build"/"Model" facts. No SDL, no `Popover`, no `TableView`: this is the
/// whole of what makes the row set testable on the host.
fn build_details_rows(r: &StorageReadout, build: &str, model: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    if let Some(class) = r.stored_sign_in {
        rows.push(("Stored sign-in".to_string(), class.to_string()));
    }
    if let Some(persist) = r.persist_outcome {
        rows.push(("Last save".to_string(), persist.to_string()));
        if let Some(reason) = r.preserve_reason {
            // Keep the decisive reason in its own row. Concatenating it with the outcome made
            // the shared right-aligned value consume the label column and paint over it.
            rows.push(("Save reason".to_string(), reason.to_string()));
        }
    }
    if let Some(key) = r.key_service {
        rows.push(("Key service".to_string(), key.to_string()));
    }
    if let Some(id) = r.key_identity {
        rows.push(("Key identity".to_string(), id.to_string()));
    }
    for c in &r.candidates {
        rows.push((candidate_category_label(c.category), format_candidate_value(c.outcome, c.errno)));
    }
    if let Some(winner) = r.cold_winner {
        rows.push(("Cold winner".to_string(), winner.to_string()));
    }
    for (label, value) in [
        ("Account credential", r.cold_account),
        ("Server credential", r.cold_pms),
        ("Server dialable", r.cold_server),
        ("Can go local", r.cold_local),
    ] {
        if let Some(value) = value {
            rows.push((label.to_string(), value.to_string()));
        }
    }
    if let Some(readback) = &r.fresh_readback {
        rows.push(("Fresh save readback".to_string(), readback.clone()));
    }
    rows.push(("Build".to_string(), build.to_string()));
    rows.push(("Model".to_string(), model.to_string()));
    rows
}

const DETAILS_TITLE: &std::ffi::CStr = c"Storage details";
const DETAILS_REPORT: &std::ffi::CStr = c"Send report";
const DETAILS_REPORT_GUIDANCE: &str =
    "Send a report if sign-in keeps failing or the app asks you to sign in again after a restart.";
const DETAILS_PANEL_W: f32 = 900.0;
const DETAILS_PANEL_RAD: f32 = 20.0;

static mut DETAILS_POP: Popover = Popover::new();
static mut DETAILS_TABLE: TableView = TableView::new();

fn details_pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(DETAILS_POP) }
}
fn details_table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(DETAILS_TABLE) }
}

/// Is the Details panel open (or still fading out)?
///
/// **`visible()`, not `is_open()`** — the same correction [`modal_open`]'s doc records for the
/// one-off report alert, and for the same failure: `dismiss()` clears `open` at once and then
/// fades for a few frames, so a second BACK during that fade would fall through to `app.rs`'s root
/// rule and hand the whole screen to the television while the panel was still on top of it. This
/// is the one predicate behind BOTH the draw's input claim and [`modal_open`], so a panel that can
/// be seen is a panel that owns the keys.
fn details_open() -> bool {
    details_pop().visible()
}

/// The panel's rect: fixed width, centred, height clamped to its measured content and the screen —
/// the same shape `item_menu::panel_rect` uses for an anchored panel, without the anchor this one
/// does not have.
/// Room above the table for the panel's own title line — shared by [`details_panel_rect`] (so the
/// title has somewhere to draw without pushing the table off the panel's bottom edge) and the title
/// draw call in [`draw`] itself, so the two cannot drift apart into an overlap or a gap.
fn details_header_h() -> f32 {
    crate::text::text_height(theme::size::LABEL, 0) + theme::space::SM
}

fn details_footer_h() -> f32 {
    TextView::new(DETAILS_REPORT_GUIDANCE, theme::size::CAPTION, theme::TEXT_SECONDARY)
        .measure_h(DETAILS_PANEL_W - 2.0 * theme::space::LG)
        + 2.0 * theme::space::SM
        + crate::ui::widgets::StatusOverlay::CTRL_H
}

fn details_panel_rect() -> Rect {
    let h = (details_table().measured_height()
        + details_header_h()
        + 2.0 * theme::space::MD
        + details_footer_h())
        .clamp(160.0, SCR_H - 2.0 * theme::space::XL);
    Rect::new(
        (SCR_W - DETAILS_PANEL_W) * 0.5,
        (SCR_H - h) * 0.5,
        DETAILS_PANEL_W,
        h,
    )
}

/// The table frame is one geometry fact shared by sizing, scrolling and drawing.  TableView's
/// update contract requires the exact same height that draw receives.
fn details_table_rect(r: Rect) -> Rect {
    details_table_rect_with_header(r, details_header_h(), details_footer_h())
}

fn details_table_rect_with_header(r: Rect, header_h: f32, footer_h: f32) -> Rect {
    let table_y = r.y + theme::space::MD + header_h;
    Rect::new(
        r.x,
        table_y,
        r.w,
        r.h
            - header_h
            - 2.0 * theme::space::MD
            - footer_h,
    )
}

fn details_report_rect(r: Rect) -> Rect {
    Rect::new(
        r.x + theme::space::LG,
        r.y + r.h - theme::space::MD - crate::ui::widgets::StatusOverlay::CTRL_H,
        Button::pill_w(DETAILS_REPORT.as_ptr(), theme::size::BODY, false),
        crate::ui::widgets::StatusOverlay::CTRL_H,
    )
}

fn storage_report_available() -> bool {
    storage_report_available_for(scene().storage_report_state, scene().storage_report_failed)
}

fn storage_report_available_for(state: Option<DeliveryState>, failed: bool) -> bool {
    failed
        || !matches!(
        state,
        Some(DeliveryState::Queued | DeliveryState::Sending | DeliveryState::Sent)
    )
}

fn storage_report_status(state: Option<DeliveryState>, failed: bool) -> Option<&'static str> {
    if failed {
        return Some("Report failed — Send report to try again");
    }
    match state {
        Some(DeliveryState::Queued) => Some("Report queued"),
        Some(DeliveryState::Sending) => Some("Sending — keep the app open"),
        Some(DeliveryState::Sent) => Some("Report sent"),
        Some(DeliveryState::Failed) => Some("Report failed — Send report to try again"),
        None => None,
    }
}

/// "Build"/"Model" for Details and reports. These technical facts intentionally do not compete
/// with the ordinary sign-in screen's user-facing status.
fn footer_build_and_model() -> (String, String) {
    let release = crate::webos::info().release_line();
    let set = crate::webos::device().set_line();
    let set = if set.is_empty() { "?".to_string() } else { set };
    (
        format!("{} \u{00B7} {}", crate::plex::identity::VERSION, release),
        set,
    )
}

fn rebuild_storage_details_table() {
    let (build, model) = footer_build_and_model();
    let rows = build_details_rows(&scene().storage_readout, &build, &model);
    let mut sec = Section::new("");
    // Put the receipt first when returning from Send, fully readable without scrolling or
    // squeezing it into the diagnostic table's value column.
    if let Some(id) = &scene().storage_report_id {
        sec = sec.row(Row::new("Report ID").detail(id.clone()));
    }
    if let Some(status) = storage_report_status(
        scene().storage_report_state,
        scene().storage_report_failed,
    ) {
        sec = sec.row(Row::new("Report status").detail(status));
    }
    for (label, value) in rows {
        sec = sec.row(Row::new(label).value(value));
    }
    details_table().compact = true;
    details_table().list_focused = false; // read-only: nothing here takes OK
    let sel = details_table().sel;
    details_table().set_sections(vec![sec], sel, true);
}

fn open_storage_details() {
    scene().details_report_rect = None;
    rebuild_storage_details_table();
    details_pop().open();
    crate::ui::idle::invalidate();
}

fn refresh_storage_details_table() {
    if !details_open() {
        return;
    }
    rebuild_storage_details_table();
    crate::ui::idle::invalidate();
}

fn close_storage_details() {
    details_pop().dismiss();
    scene().details_report_rect = None;
    crate::ui::idle::invalidate();
}

fn open_storage_report_confirmation() {
    let (attempt, _) = auth::storage_report_context();
    details_pop().close();
    scene().details_report_rect = None;
    scene().report_prompt_storage_attempt = Some(attempt);
    scene().report_prompt_warning = auth::pending_persistence_warning().map(|w| w.key);
    // This attempt has already offered a report. A network failure arriving behind this
    // confirmation must not immediately ask again after the user chooses Not now.
    scene().report_offered_for = Some(attempt);
    scene()
        .report_alert
        .open_with_body(REPORT_STORAGE_QUESTION, REPORT_BODY);
    crate::ui::idle::invalidate();
}

/// Which of the storage action row's two pills has focus — `0` = Try again, `1` = Details. A plain
/// module `static` rather than a [`Scene`] field: it is UI-only navigation state with no bearing on
/// what the screen knows, the same kind of thing [`STORAGE_RETRY_STATE`] already is.
static STORAGE_FOCUS: AtomicUsize = AtomicUsize::new(0);
/// `0` = the "Try again" pill.
const STORAGE_FOCUS_RETRY: usize = 0;
/// `1` = the "Details" pill.
const STORAGE_FOCUS_DETAILS: usize = 1;

const STORAGE_DETAILS: &std::ffi::CStr = c"Details";
const STORAGE_NEW_CODE: &std::ffi::CStr = c"New code";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoragePrimaryAction {
    Continue(auth::PersistenceWarningKey),
    RetrySecureOpen,
    NewCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StorageAction {
    Continue(auth::PersistenceWarningKey),
    RetrySecureOpen,
    NewCode,
    Details,
}

static ARMED_STORAGE_ACTION: std::sync::Mutex<Option<(StorageAction, u64)>> =
    std::sync::Mutex::new(None);

fn storage_primary_for(
    waiting: bool,
    secure_unavailable: bool,
    new_code_ready: bool,
) -> Option<StoragePrimaryAction> {
    if !waiting {
        None
    } else if secure_unavailable {
        Some(StoragePrimaryAction::RetrySecureOpen)
    } else if new_code_ready {
        Some(StoragePrimaryAction::NewCode)
    } else {
        None
    }
}

fn storage_primary_action() -> Option<StoragePrimaryAction> {
    if let Some(warning) = auth::pending_persistence_warning() {
        return Some(StoragePrimaryAction::Continue(warning.key));
    }
    storage_primary_for(
        storage_action_showing(),
        crate::plex::session::secure_unavailable(),
        qr_escape_offered(scene().phase_ms),
    )
}

fn storage_focus_for(primary: Option<StoragePrimaryAction>, stored: usize) -> usize {
    if primary.is_some() { stored.min(STORAGE_FOCUS_DETAILS) } else { STORAGE_FOCUS_DETAILS }
}

fn focused_storage_action() -> StorageAction {
    if STORAGE_FOCUS.load(Ordering::Acquire) == STORAGE_FOCUS_DETAILS {
        StorageAction::Details
    } else {
        match storage_primary_action() {
            Some(StoragePrimaryAction::Continue(key)) => StorageAction::Continue(key),
            Some(StoragePrimaryAction::RetrySecureOpen) => StorageAction::RetrySecureOpen,
            Some(StoragePrimaryAction::NewCode) => StorageAction::NewCode,
            None => StorageAction::Details,
        }
    }
}

pub(crate) fn arm_storage_action() -> bool {
    if !storage_action_showing() || modal_open() {
        return false;
    }
    *ARMED_STORAGE_ACTION.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((focused_storage_action(), auth::current_attempt()));
    true
}

fn armed_storage_action_valid(
    action: StorageAction,
    armed_attempt: u64,
    live_attempt: u64,
    waiting: bool,
    secure_unavailable: bool,
    new_code_ready: bool,
    modal_visible: bool,
) -> bool {
    !modal_visible
        && waiting
        && armed_attempt == live_attempt
        && match action {
            StorageAction::Continue(_) => false, // validated against the exact live warning below
            StorageAction::Details => true,
            StorageAction::RetrySecureOpen => secure_unavailable,
            StorageAction::NewCode => new_code_ready && !secure_unavailable,
        }
}

fn warning_action_valid(
    action: StorageAction,
    armed_attempt: u64,
    live: auth::PersistenceWarningKey,
    modal_visible: bool,
) -> bool {
    !modal_visible && armed_attempt == live.attempt && match action {
        StorageAction::Continue(key) => key == live,
        StorageAction::Details => true,
        _ => false,
    }
}

/// PURE. Details is always available on the waiting QR screen, including a clean first install;
/// the panel then shows the fixed build/model facts and any storage facts known so far.
fn storage_details_pill_shown(r: &StorageReadout) -> bool {
    let _ = r;
    true
}

/// PURE. The line under the control, or `None` when the read-out has nothing to add to it. The
/// three states are three different sentences because they answer three different questions:
/// whether anything is happening, whether it worked, and — the one that has to be said out loud —
/// that a second failure is not a dead end, because the code beside it still is a way in.
///
/// `&'static str`, not a `CStr`: [`draw_storage_action`] draws this through [`TextView`] now
/// (review finding — see [`storage_note_room`]), which wants a `str` to measure and wrap, and
/// nothing else here still needed the raw-pointer shape.
fn storage_note(state: u8) -> Option<&'static str> {
    match state {
        RETRY_BUSY => Some("Asking the television\u{2019}s key service\u{2026}"),
        RETRY_FAILED => Some("Still no answer. You can scan the code to sign in again."),
        _ => None,
    }
}

/// **Claim the one worker slot for this press.** `true` once, for the press that takes it; `false`
/// for a press that arrives while the slot is already spoken for.
///
/// **Claimed from either SETTLED state — [`RETRY_IDLE`] and [`RETRY_FAILED`]** (review finding,
/// 2026-09-11). It used to claim from `RETRY_IDLE` alone, which reads as the same no-stacking rule
/// and is not: a worker that got nowhere leaves the slot at `RETRY_FAILED` and nothing on this
/// route ever puts it back (`enter` does, but that is the NEXT visit), so from the second press
/// onward the only control on the screen did nothing at all, silently, for the rest of the visit —
/// on the one screen whose subject is a television that seems stuck, beside a note that invites
/// exactly that second press. Asking a service that did not answer whether it will answer NOW is
/// the whole point of the control.
///
/// The two states it still refuses are the two in-flight ones: [`RETRY_BUSY`] (a worker is asking)
/// and [`RETRY_OPENED`] (one succeeded and [`settle_storage_retry`] has not applied it yet). That
/// is the rule that was always right — leaning on OK must not spawn a worker per frame against a
/// service whose whole problem is that it answers slowly.
fn claim_storage_retry() -> bool {
    use std::sync::atomic::Ordering;
    [RETRY_IDLE, RETRY_FAILED].iter().any(|&from| {
        STORAGE_RETRY_STATE
            .compare_exchange(from, RETRY_BUSY, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    })
}

/// Ask the key service again, off the main thread — the *Try again* press.
///
/// Refuses to stack: a second press while one is IN FLIGHT is swallowed rather than queued (see
/// [`claim_storage_retry`], which is also why a press after a FAILED one is not). A refused spawn
/// (`task::spawn_small` returning `false` under EAGAIN — see `task.rs`) falls back to asking
/// inline: a frozen frame is worse than nothing, but a button that silently does nothing at all is
/// worse than both.

fn start_storage_retry() {
    use std::sync::atomic::Ordering;
    if !claim_storage_retry() {
        return;
    }
    // Captured BEFORE the worker starts, under the same compare-exchange that claims the press —
    // see [`STORAGE_RETRY_ATTEMPT`]'s own doc for why the settle needs it.
    STORAGE_RETRY_ATTEMPT.store(auth::current_attempt(), Ordering::Release);
    crate::ui::idle::invalidate();
    let ask = || {
        let opened = crate::plex::session::retry_secure_open();
        STORAGE_RETRY_STATE.store(
            if opened { RETRY_OPENED } else { RETRY_FAILED },
            Ordering::Release,
        );
        crate::ui::idle::invalidate();
    };
    if !crate::task::spawn_small("keyretry", ask) {
        ask();
    }
}

/// PURE. Should an opened storage envelope still be applied to the flow, or has it gone stale?
///
/// `captured` is [`STORAGE_RETRY_ATTEMPT`] — the attempt id the press was about; `now_phase` and
/// `now_attempt` are what the flow is AT SETTLE TIME. Split out from [`settle_storage_retry`] so
/// the race it guards against is a fact a host test can assert directly, with no worker, no `Ctl`
/// and no dependence on `auth`'s process-global state.
///
/// **Both checks are required, and neither alone is enough.** The attempt id alone misses a
/// same-attempt race: a QR sign-in that completes NORMALLY (the phone's own poll landing while the
/// television is still asking its key service about a different, older account) never bumps
/// `Ctl::attempt` — only a fresh reset does — so a worker that returns after that still reads a
/// matching id while `Ctl` already holds the newly-signed-in account. The phase alone misses the
/// other direction: a BACK-then-retry that lands back on `Waiting` (a fresh reset, e.g. via
/// `auth::retry`) is phase-equal to the press's own wait but is a different attempt entirely, and
/// applying the old envelope over it would resurrect an account the user had already moved past.
/// Requiring `Phase::Waiting` — the one phase the storage read-out draws over — is what makes
/// "still on screen" and "still this attempt" the same claim from two directions.
fn storage_retry_still_applies(captured: u64, now_phase: Phase, now_attempt: u64) -> bool {
    now_phase == Phase::Waiting && now_attempt == captured
}

/// **Main-thread half of [`start_storage_retry`]: an opened envelope becomes a running app.**
///
/// The worker can only publish the session; entering it registers servers, installs a PMS client
/// and picks a profile, all of which belong to the loop. `auth::resume_secure_session` puts the
/// flow exactly where the boot gate would have — the who's-watching picker, or `Phase::Ready` for
/// the main loop's own `take_ready` to install — so from the next frame this launch is
/// indistinguishable from one whose key service answered the first time.
///
/// **Review finding: a worker's success is stale the instant the flow it was asked about has
/// moved on, and this used to act on it regardless.** [`storage_retry_still_applies`] is the gate:
/// a flow that has moved on wins either way — the opened envelope is simply not applied, `Ctl` is
/// left exactly as the newer result made it, and this state resets to idle rather than
/// [`RETRY_FAILED`], because nothing here failed — the read-out is just about to leave.
fn settle_storage_retry() {
    use std::sync::atomic::Ordering;
    if STORAGE_RETRY_STATE.load(Ordering::Acquire) != RETRY_OPENED {
        return;
    }
    let captured_attempt = STORAGE_RETRY_ATTEMPT.load(Ordering::Acquire);
    if !storage_retry_still_applies(captured_attempt, auth::phase(), auth::current_attempt()) {
        // The flow this press was about is no longer the one on screen — a newer sign-in (this
        // same attempt reaching Ready, or a fresh one entirely) already won. Applying the opened
        // envelope now would silently replace it with the stale stored account.
        STORAGE_RETRY_STATE.store(RETRY_IDLE, Ordering::Release);
        crate::log(
            "login: storage retry opened the envelope after a newer sign-in already won — dropped",
        );
        return;
    }
    let resumed = crate::auth::resume_secure_session(crate::plex::session::snapshot());
    STORAGE_RETRY_STATE.store(
        if resumed { RETRY_IDLE } else { RETRY_FAILED },
        Ordering::Release,
    );
    crate::ui::idle::invalidate();
}

/// PURE. Vertical room actually left for the retry note, above the pill.
///
/// **Review finding, 2026-09-10**: this note used to draw at a fixed one-line offset from the
/// pill, on the strength of a comment claiming it "borrows" the `space::XL` gap `draw_narrative`
/// already leaves between its own body copy and the action slot
/// (`RouteLayout::draw_narrative_with_note`'s `copy_bottom = action.y - space::XL`) — but nothing
/// in the fixed offset actually measured that gap, so once the pill was lifted clear of the footer
/// the note's fixed offset from the PILL routinely ran past `copy_bottom`
/// and into the body's own territory, overprinting it whenever the body filled its allowance. The
/// fix mirrors `draw_narrative_with_note`'s own rule for its body's `max_lines` (route_screen.rs):
/// derive the cap from what is actually LEFT, never assume a constant fits.
///
/// The room is bounded above by `copy_bottom` (the body's own reserved floor) and below by the
/// pill's own top, `space::SM` clear of it — the same gap [`draw_storage_action`] already used to
/// place a fixed-height note, now spent as a budget instead of an offset.
fn storage_note_room(action: Rect, r: Rect) -> f32 {
    let copy_bottom = action.y - theme::space::XL;
    (r.y - theme::space::SM - copy_bottom).max(0.0)
}

/// The storage read-out's control and its note, in the shared bottom action slot of the narrative
/// column — the same slot `ui::onboard` and `ui::consent` put their own commit control in, rather
/// than a position of this screen's own.
fn draw_storage_action(p: Painter, env: &Env, layout: RouteLayout) {
    let primary = storage_primary_action();
    let show_retry = primary.is_some();
    let show_details = storage_details_pill_shown(&scene().storage_readout);
    // Clamp rather than trust the stored focus: a readout that arrives AFTER the panel already
    // rendered with focus on slot 1 could otherwise leave a hidden pill "focused" — impossible in
    // practice (the pill can only ever have been reached by first being shown), but the clamp is
    // one line and turns a would-be invariant into a proven one.
    let focus = storage_focus_for(primary, STORAGE_FOCUS.load(Ordering::Acquire));
    let y = layout.action.y;
    unsafe { (*addr_of_mut!(STORAGE_ACTION_POP)).clear() };
    let mut x = layout.action.x;
    if show_retry {
        let label = match primary.unwrap() {
            StoragePrimaryAction::Continue(_) => c"Continue",
            StoragePrimaryAction::RetrySecureOpen => STORAGE_RETRY,
            StoragePrimaryAction::NewCode => STORAGE_NEW_CODE,
        };
        let retry_w = Button::pill_w(label.as_ptr(), theme::size::BODY, false);
        let r = Rect::new(x, y, retry_w.min(layout.action.w), layout.action.h);
        Button::new(label.as_ptr(), theme::size::BODY, r)
            .focused(focus == STORAGE_FOCUS_RETRY)
            .scale(unsafe { (*addr_of_mut!(STORAGE_ACTION_POP)).scale(STORAGE_FOCUS_RETRY) })
            .draw(env, p);
        unsafe { (*addr_of_mut!(STORAGE_ACTION_POP)).place(STORAGE_FOCUS_RETRY, r) };
        x += r.w + theme::space::SM;
    }
    if show_details {
        let details_w = Button::pill_w(STORAGE_DETAILS.as_ptr(), theme::size::BODY, false);
        let dr = Rect::new(x, y, details_w, layout.action.h);
        Button::new(STORAGE_DETAILS.as_ptr(), theme::size::BODY, dr)
            .focused(focus == STORAGE_FOCUS_DETAILS)
            .scale(unsafe { (*addr_of_mut!(STORAGE_ACTION_POP)).scale(STORAGE_FOCUS_DETAILS) })
            .draw(env, p);
        unsafe { (*addr_of_mut!(STORAGE_ACTION_POP)).place(STORAGE_FOCUS_DETAILS, dr) };
    }
    let Some(note) = matches!(primary, Some(StoragePrimaryAction::RetrySecureOpen))
        .then(|| storage_note(STORAGE_RETRY_STATE.load(std::sync::atomic::Ordering::Acquire)))
        .flatten()
    else {
        return;
    };
    let line_h = crate::text::text_height(theme::size::CAPTION, 0);
    // ABOVE the pill, not under it. The budget is the room actually LEFT above the pill, never a fixed
    // one-line assumption — see [`storage_note_room`] for why that used to overprint the body.
    let room = storage_note_room(
        layout.action,
        Rect::new(layout.action.x, y, layout.action.w, layout.action.h),
    );
    let leading = theme::size::CAPTION as f32 + theme::space::XS;
    let max_lines = ((room / leading).floor() as usize).clamp(1, 2);
    let note_view = TextView::new(note, theme::size::CAPTION, theme::TEXT_SECONDARY)
        .leading(leading)
        .max_lines(max_lines);
    // Bottom-aligned to the pill and clamped to the room itself, so a `max_lines` rounded up by
    // the `.clamp(1, …)` floor above still cannot draw above `copy_bottom` — the two-line minimum
    // exists so a genuinely tiny room still shows SOMETHING rather than silence, at the cost of a
    // partial overlap the room could not avoid either way.
    let note_h = note_view.measure_h(layout.action.w).min(room.max(line_h));
    let bottom = y - theme::space::SM;
    note_view.draw(
        p,
        Rect::new(layout.action.x, bottom - note_h, layout.action.w, note_h),
    );
}

/// The verb on both the failed and the stuck read-out, because it is the same call underneath.
///
/// `auth::retry` bumps the auth epoch, so a worker still blocked in the wedged request has its
/// result discarded when it finally returns, and it re-runs only the leg that failed — discovery
/// when the pin already yielded an account credential, a whole fresh pin when it did not.
const ESCAPE: &std::ffi::CStr = c"Try again";

/// The storage read-out's one control's own press surface — the shared type every route-family
/// action row uses ([`ActionRow`]) rather than a bespoke `Button` draw with no focus pop at all.
///
/// **Review finding, 2026-09-10**: this pill was the first control this screen puts in
/// `RouteLayout::action`, and it drew as a flat `Button` at a constant scale — every sibling pill
/// on this route family (Settings' Retry/Done, first-run consent's two answers) already animates
/// through one of these, so this one alone read as dead on arrival: focus moved onto it with no
/// grow, the one motion `CtlPop::step`/`::scale` give every other control the instant they are
/// wired at all (`widgets.rs`'s own doc: "the pop is the tile's spring now"). Wiring `.step()` in
/// [`update`] and `.scale(0)` in [`draw_storage_action`] fixes exactly that.
///
/// **What this does NOT reach: the press DIP itself.** `CtlPop::scale`'s own doc says the dip is
/// folded in "once the caller has armed `press::begin_ctl` on its OK-down" — and arming is the
/// caller's job specifically because [`crate::ui::press`] is ONE GLOBAL sequencer app.rs drives:
/// arm on the key-DOWN that takes a control face, release on the matching key-UP (gated on the
/// SAME local `ok_armed` app.rs's own dispatcher sets), poll for the deferred commit. Every other
/// route-family pill arms it from app.rs's per-route keydown ladder (see `Route::Onboard`'s and
/// `Route::Profiles`' own arms beside this route's). This screen's OK dispatch is the one
/// fallback case that still calls straight into [`key`] with no matching arm — and this fix's
/// scope is `ui/login.rs`/`auth.rs` only, so wiring that ladder entry is left for whoever next
/// touches `app.rs`'s `key_onboarding`, rather than armed here with no release to pair it: a
/// `begin_ctl` this module called with nothing setting app.rs's `ok_armed` would leave
/// `press::State` latched at `Phase::Down` forever, breaking the dip for every OTHER control in
/// the app after the first press on this one — a worse bug than the missing dip it would "fix".
/// The focus pop above is real and device-verified (`docs/measurements/
/// keymanager-unavailable-retry-pill-sim-2026-09-10.md`); the ring-on-press is a follow-up.
static mut STORAGE_ACTION_POP: ActionRow<2> = ActionRow::new();

static mut SCENE: Option<Scene> = None;

fn scene() -> &'static mut Scene {
    unsafe {
        (*addr_of_mut!(SCENE))
            .as_mut()
            .expect("login::init not called")
    }
}

pub fn init() {
    let mut report_alert = DecisionAlert::new();
    // The "Send report" answer ends nothing — it is the opposite of the delete alert's
    // `Tone::Destructive` default, and shipping it red would say otherwise.
    report_alert.set_tone(Tone::Neutral);
    unsafe {
        *addr_of_mut!(SCENE) = Some(Scene {
            spin_ms: 0.0,
            qr_tex: 0,
            qr_gen: auth::qr_generation(),
            ground: RouteGround::new(),
            phase_ms: 0.0,
            wait: wait_id(),
            delete_leftovers: 0,
            last_link_detail: None,
            link_detail_repaint_pending: false,
            report_alert,
            report_offered_for: None,
            report_send_failed: false,
            storage_readout: StorageReadout::default(),
            storage_report_attempt: None,
            storage_report_id: None,
            storage_report_state: None,
            storage_report_failed: false,
            report_prompt_storage_attempt: None,
            report_prompt_warning: None,
            persistence_warning_seen: None,
            details_report_rect: None,
        });
    }
}

/// Mount the auth route without replacing the cached QR texture. A fresh auth flow invalidates the
/// texture from [`update`] when it reaches `Creating`; this only resets the visit's visual ground.
pub fn enter() {
    let s = scene();
    s.ground.reset();
    // A fresh visit is a fresh wait, whatever phase the last one died in.
    s.phase_ms = 0.0;
    s.wait = wait_id();
    // **Issue #75 review.** A one-off report alert left OPEN when the flow leaves this route
    // (the link recovers and finishes the sign-in right after `note_waiting_trouble` opened it,
    // with nobody dismissing it) would otherwise reappear over the NEXT visit's fresh QR screen —
    // a sign-out then a re-entry here — and swallow every key exactly as it does while genuinely
    // open. `close()`, not `dismiss()`: an instant hide is right for "the subject vanished out
    // from under the alert" (`Popover::close`'s own case), which this is — the trouble this alert
    // was about belongs to the attempt that just ended.
    s.report_alert.close();
    s.link_detail_repaint_pending = false;
    // A "still no answer" note belongs to the press that earned it, not to the next visit to this
    // route — a sign-out and a re-entry must not open on somebody else's failed retry. Only the
    // settled failure is cleared: a retry still in flight is still in flight.
    let _ = STORAGE_RETRY_STATE.compare_exchange(
        RETRY_FAILED,
        RETRY_IDLE,
        std::sync::atomic::Ordering::AcqRel,
        std::sync::atomic::Ordering::Relaxed,
    );
    // A Details panel left open belongs to the visit that opened it, for the same reason the report
    // alert above is `close()`d rather than `dismiss()`d; focus goes back to `Try again` for the
    // fresh visit's own first frame.
    details_pop().close();
    scene().storage_report_attempt = None;
    scene().storage_report_id = None;
    scene().storage_report_state = None;
    scene().storage_report_failed = false;
    scene().report_prompt_storage_attempt = None;
    scene().report_prompt_warning = None;
    scene().persistence_warning_seen = None;
    scene().details_report_rect = None;
    STORAGE_FOCUS.store(STORAGE_FOCUS_RETRY, Ordering::Release);
    // The facts this visit shows are THIS visit's. A sign-out and a re-entry, or a "Sign in" from
    // the account menu on a running app, both land here after the save/read state has moved on
    // from whatever the boot's own refresh recorded.
    refresh_storage_readout();
    crate::ui::idle::invalidate();
}

/// Tear down route-owned modals before the app hands the route to Home, Profiles or Onboard.
/// `Popover::close` is required here: the route no longer draws the fade, so leaving it in the
/// global open counter would leak modal ownership for the rest of the process.
pub fn leave() {
    scene().report_alert.close();
    scene().link_detail_repaint_pending = false;
    details_pop().close();
    scene().storage_report_attempt = None;
    scene().storage_report_id = None;
    scene().storage_report_state = None;
    scene().storage_report_failed = false;
    scene().report_prompt_storage_attempt = None;
    scene().report_prompt_warning = None;
    scene().persistence_warning_seen = None;
    scene().details_report_rect = None;
    STORAGE_FOCUS.store(STORAGE_FOCUS_RETRY, Ordering::Release);
    unsafe { (*addr_of_mut!(STORAGE_ACTION_POP)).clear() };
    *ARMED_STORAGE_ACTION.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Decide whether a changed link-health sentence should repaint the QR host now. While the report
/// alert is visible—including its dismissal fade—the sentence is fully covered and a repaint
/// would needlessly break the settled host snapshot for one frame. Multiple poll changes collapse
/// into one repaint when the modal is gone; sampling itself never stops.
fn link_detail_repaint(changed: bool, alert_visible: bool, pending: bool) -> (bool, bool) {
    if alert_visible {
        return (false, pending || changed);
    }
    (changed || pending, false)
}

pub fn update(dt: f32) {
    let warning = auth::pending_persistence_warning().map(|w| w.key);
    if scene().persistence_warning_seen != warning {
        scene().persistence_warning_seen = warning;
        scene().report_alert.close();
        details_pop().close();
        scene().report_prompt_storage_attempt = None;
        scene().report_prompt_warning = None;
        scene().storage_report_id = None;
        scene().storage_report_state = None;
        scene().storage_report_failed = false;
        scene().details_report_rect = None;
        STORAGE_FOCUS.store(STORAGE_FOCUS_RETRY, Ordering::Release);
        refresh_storage_readout();
        crate::ui::idle::invalidate();
    }
    let s = scene();
    s.spin_ms += dt * 1000.0;
    let live_attempt = auth::current_attempt();
    if scene().storage_report_attempt != Some(live_attempt) {
        scene().storage_report_attempt = None;
        scene().storage_report_id = None;
        scene().storage_report_state = None;
        scene().storage_report_failed = false;
    } else if let Some(id) = scene().storage_report_id.clone() {
        if let Some(next) = oneoff::delivery_state(&id) {
            if Some(next) != scene().storage_report_state {
                scene().storage_report_state = Some(next);
                scene().storage_report_failed = next == DeliveryState::Failed;
                if details_open() {
                    refresh_storage_details_table();
                }
                crate::ui::idle::invalidate();
            }
        }
    }
    settle_storage_retry();
    // A settled *Try again* changes every fact the Details panel shows — see [`STORAGE_RETRY_SEEN`].
    // After `settle_storage_retry`, so a press that OPENED the envelope is observed at the state it
    // settled to rather than mid-handover.
    let retry_state = STORAGE_RETRY_STATE.load(Ordering::Acquire);
    if STORAGE_RETRY_SEEN.swap(retry_state, Ordering::AcqRel) != retry_state
        && retry_state != RETRY_BUSY
    {
        refresh_storage_readout();
    }
    // The storage action row's focused slot — `Try again` until a `Details` pill exists to move to
    // (`storage_details_pill_shown`), and clamped back to it if the readout that grew the row
    // shrinks again (defensive; it never actually does once set — see `StorageReadout`'s doc).
    if auth::phase() == Phase::Waiting && storage_primary_action().is_none() {
        STORAGE_FOCUS.store(STORAGE_FOCUS_DETAILS, Ordering::Release);
    }
    let focused_slot = storage_action_showing().then(|| {
        storage_focus_for(storage_primary_action(), STORAGE_FOCUS.load(Ordering::Acquire))
    });
    unsafe {
        (*addr_of_mut!(STORAGE_ACTION_POP)).step(focused_slot, dt);
    }
    if details_pop().visible() {
        // This panel's own springs, kept out of the host page's motion — the same
        // `popover::own_motion` scope every compact menu in the app takes.
        let _own = crate::ui::popover::own_motion();
        details_pop().update(dt);
        let r = details_panel_rect();
        details_table().update(dt, details_table_rect(r).h);
    }
    // Each wait gets its own clock. A flow that walks Creating → Waiting → Discovering is making
    // progress, and restarting the timer at every step is what stops a slow-but-healthy sign-in
    // from being offered a way out of itself.
    let live = wait_id();
    if wait_restarted(s.wait, live) {
        s.wait = live;
        s.phase_ms = 0.0;
    } else {
        s.phase_ms += dt * 1000.0;
    }
    drop_a_stale_qr(s, auth::qr_generation());
    // `link_detail` is sampled from a mutex updated roughly once a poll (~2 s), not from a spring
    // `ui::idle` can already see — so this screen must report the discrete change itself, exactly
    // as `Xfade::tick`/`Spinner::draw` do for their own clocks. Comparing against the LAST DRAWN
    // sentence (not e.g. a bare `unanswered` counter) is what stops a healthy link — where every
    // poll answers and `link_detail` stays `None` forever — from invalidating every frame.
    let detail = auth::link_detail(&auth::link_state());
    let changed = link_detail_changed(&s.last_link_detail, &detail);
    if changed {
        s.last_link_detail = detail;
    }
    let (invalidate_link_detail, pending) = link_detail_repaint(
        changed,
        s.report_alert.visible(),
        s.link_detail_repaint_pending,
    );
    s.link_detail_repaint_pending = pending;
    if invalidate_link_detail {
        crate::ui::idle::invalidate();
    }
    s.report_alert.update(dt);
    // **Issue #75.** While stuck (not merely erroring), note the trouble only once the screen's
    // own stalled-wait escape is already on offer — the same gate as the escape itself, so the
    // report alert and the New code action arrive together rather than the alert jumping the
    // gun on a sign-in that is still perfectly healthy.
    if qr_escape_ready(s) && auth::link_unreachable(&auth::link_state()) {
        auth::note_waiting_trouble();
    }
    if warning.is_none() && auto_report_offer_allowed(
        scene().report_prompt_storage_attempt.is_some(),
        details_open(),
    ) {
        if let Some(attempt) = offer_alert(
            s.report_offered_for,
            auth::trouble_snapshot().map(|(a, _, reported)| (a, reported)),
        ) {
        s.report_offered_for = Some(attempt);
        // A failure to send belongs to the press that failed, not to whatever this attempt does
        // next — reset so a stale "couldn't be sent" from an earlier trouble cannot linger over
        // an alert that has not been pressed yet.
        s.report_send_failed = false;
        s.report_alert
            .open_with_body(c"Send a report about this sign-in problem?", REPORT_BODY);
        }
    }
}

/// **Issue #75.** PURE. Should the one-off report alert open now? `offered_for` is the attempt
/// [`Scene::report_alert`] was last opened (or answered) for; `snapshot` is `(attempt,
/// auto_reported)` from [`auth::trouble_snapshot`] when a trouble exists for the CURRENT attempt.
/// Returns the attempt id to open for, or `None` to leave the alert alone.
///
/// Never a bool — comparing attempt IDs rather than "is there a trouble right now" is what stops a
/// fresh flow reset (whose new attempt starts with no trouble at all, then earns one) from being
/// read as "still the trouble already answered".
fn offer_alert(offered_for: Option<u64>, snapshot: Option<(u64, bool)>) -> Option<u64> {
    let (attempt, auto_reported) = snapshot?;
    if auto_reported || offered_for == Some(attempt) {
        return None;
    }
    Some(attempt)
}

fn auto_report_offer_allowed(manual_prompt_open: bool, details_visible: bool) -> bool {
    !manual_prompt_open && !details_visible
}

/// **Issue #75.** PURE. The caption drawn once a trouble has actually left the television — by
/// either path, the automatic standing-consent send or the one-off press — or, once a deliberate
/// "Send report" press has failed to queue, a caption saying so. `None` while neither is true.
/// `sent` wins over `send_failed` (a trouble the standing path already reported is "sent" even if
/// a later one-off press against a NEW trouble failed to queue — the two can never both be true
/// for the same trouble, since `send_trouble_once` only runs when `!reported`, but the precedence
/// is stated here rather than left as an accident of argument order).
fn report_note(sent: bool, send_failed: bool) -> Option<&'static std::ffi::CStr> {
    if sent {
        Some(c"A report about this sign-in was queued \u{2014} thank you.")
    } else if send_failed {
        Some(c"That report couldn\u{2019}t be sent right now.")
    } else {
        None
    }
}

/// The current attempt's report note, if one is owed — the one call site both [`draw_failed`] and
/// [`draw_waiting`] use, so the two can never disagree about what "sent" (or "failed to send")
/// means. `send_failed` is per-screen-instance state (reset per attempt in [`update`]), not part
/// of `auth::Trouble`, because it describes a UI press outcome the auth layer never needed to know.
fn current_report_note(send_failed: bool) -> Option<&'static std::ffi::CStr> {
    if scene().storage_report_attempt == Some(auth::current_attempt()) {
        return match scene().storage_report_state {
            Some(DeliveryState::Sending) => Some(c"Sending — keep the app open."),
            Some(DeliveryState::Failed) => Some(c"That report couldn’t be sent right now."),
            Some(DeliveryState::Queued) => Some(c"That report was queued."),
            Some(DeliveryState::Sent) => Some(c"That report was sent."),
            None => None,
        };
    }
    let sent = auth::trouble_snapshot().is_some_and(|(_, _, reported)| reported);
    report_note(sent, send_failed)
}

/// The complete one-off disclosure: when the report is useful, its bounded diagnostic categories,
/// its explicit exclusions, and the event-specific (not user/device) receipt ID.
const REPORT_BODY: &str = "Use Send report if sign-in keeps failing or the app asks you to sign in again after a restart. The report includes sign-in, connection, storage and key-service diagnostics; rounded timings and try counts; and the TV model, webOS and app versions. It never includes your account name, tokens, PIN, sign-in code or network addresses. A random report ID identifies only this report, not you or this television.";

const REPORT_STORAGE_QUESTION: &std::ffi::CStr =
    c"Send a report about this television’s saved sign-in?";

/// Is a modal on this route open (or still fading out)? `app.rs` checks this before its
/// onboarding-screen root BACK rule fires — so the modal can claim BACK for itself instead of
/// backing the whole sign-in out — and before it arms the storage read-out's *Try again* press on
/// an OK, so a modal standing over that pill takes the press instead of the pill underneath it.
///
/// **Two modals, one predicate** (issue #76's report lane): the one-off report alert and the
/// storage Details panel. Neither is this screen's root. Their presentations are exclusive:
/// automatic offers wait for Details to finish closing, and an explicit Send closes Details
/// before opening its confirmation.
///
/// **`visible()`, not `is_open()`.** `DecisionAlert::dismiss` sets `open = false` at once and
/// then fades for a few frames — `is_open()` alone treats that fade as "closed", so a second BACK
/// during it fell through to the root rule and handed the whole screen to the television
/// (`webos::go_home`) while the alert was still visibly on top of it. `visible()` claims input for
/// the remainder of the fade instead, and [`key`]'s own alert branch (below) does the same.
pub fn modal_open() -> bool {
    scene().report_alert.visible() || details_open()
}

/// **Issue #75 review.** A pointer click at `(mx, my)` while the one-off report alert is open:
/// hits an answer and, when it does, performs exactly the action [`key`]'s `is_ok` branch performs
/// — send (if the hit answer is `Destructive`) then dismiss — refusing entirely when the click
/// misses both buttons.
///
/// **This exists because `app.rs`'s `Route::Login` pointer arm used to synthesize a bare OK key
/// for every click on this route**, which was harmless while the only target was "Try again" but,
/// once this alert could open, meant a click ANYWHERE on the screen — the scrim, outside the
/// panel, a click aimed at "Not now" — activated whichever answer the last D-pad press had
/// focused. Gated on [`DecisionAlert::settled`] for the reason every other pointer caller in this
/// app gates on it (`ui::route_screen`'s rule 11): a fast click during the entrance spring must
/// not land on a panel that is still displaced and nearly invisible.
pub fn alert_press_at(mx: f32, my: f32) -> bool {
    let s = scene();
    if !s.report_alert.is_open() || !s.report_alert.settled() {
        return false;
    }
    if !s.report_alert.press_at(mx, my) {
        return false;
    }
    if s.report_alert.choice() == Choice::Destructive {
        submit_report_confirmation();
    } else {
        s.report_prompt_storage_attempt = None;
        s.report_alert.dismiss();
    }
    true
}

fn record_storage_report_failure(attempt: u64) {
    let s = scene();
    s.storage_report_attempt = Some(attempt);
    // A rejected retry produced no new event. Never present the previous failed event's ID
    // as the receipt for this submission.
    s.storage_report_id = None;
    s.storage_report_failed = true;
    s.storage_report_state = Some(DeliveryState::Failed);
}

fn submit_report_confirmation() {
    let storage_attempt = scene().report_prompt_storage_attempt.take();
    let warning = scene().report_prompt_warning.take();
    if let Some(attempt) = storage_attempt {
        let ctx = match warning {
            Some(key) => auth::persistence_warning_report_context(key),
            None => Some(auth::storage_report_context().1),
        };
        match ctx.and_then(|ctx| auth::send_storage_report(attempt, ctx)) {
            Some(id) => {
                scene().storage_report_attempt = Some(attempt);
                scene().storage_report_id = Some(id);
                scene().storage_report_state = scene()
                    .storage_report_id
                    .as_deref()
                    .and_then(oneoff::delivery_state)
                    .or(Some(DeliveryState::Queued));
                scene().storage_report_failed =
                    scene().storage_report_state == Some(DeliveryState::Failed);
            }
            None => {
                record_storage_report_failure(attempt);
            }
        }
        scene().report_alert.close();
        // Returning from an explicit Send is a new result view: show its receipt first.
        // Background delivery updates preserve the user's subsequent scroll position.
        details_table().sel = 0;
        open_storage_details();
        return;
    }
    let attempt = auth::current_attempt();
    if let Some(id) = auth::send_trouble_event_once() {
        scene().storage_report_attempt = Some(attempt);
        scene().storage_report_id = Some(id);
        scene().storage_report_state = scene()
            .storage_report_id
            .as_deref()
            .and_then(oneoff::delivery_state)
            .or(Some(DeliveryState::Queued));
        scene().storage_report_failed =
            scene().storage_report_state == Some(DeliveryState::Failed);
        scene().report_send_failed = false;
    } else {
        scene().report_send_failed = true;
    }
    scene().report_alert.dismiss();
}

pub fn details_report_press_at(mx: f32, my: f32) -> bool {
    if !details_open()
        || !details_pop().is_open()
        || !details_pop().appear_settled()
        || !storage_report_available()
    {
        return false;
    }
    let Some(rect) = scene().details_report_rect else {
        return false;
    };
    if !rect.contains(mx, my) {
        return false;
    }
    open_storage_report_confirmation();
    true
}

/// A visible Details panel is the topmost modal. It is read-only, but must still claim pointer
/// clicks so a covered report alert can never receive a press through it.
pub fn details_press_at(_mx: f32, _my: f32) -> bool {
    if !details_open() {
        return false;
    }
    let _ = details_report_press_at(_mx, _my);
    true
}

/// PURE. Has the sentence the sign-in screen draws under its status changed since the last frame?
/// Trivial today (`!=`), but factored out because it is the one thing standing between a settled
/// FAILED read-out and a per-frame `ui::idle::invalidate()` — a later refinement (e.g. rounding
/// `failing_for` to the second so the trailing "…, 41 s" does not itself force a wake every
/// second) belongs here, not inlined into [`update`].
fn link_detail_changed(prev: &Option<String>, next: &Option<String>) -> bool {
    prev != next
}

/// Release the cached QR texture as soon as it stops describing the code the flow is showing.
///
/// **Keyed on [`auth::qr_generation`], not on the phase, and that swap is this screen's half of
/// issue #30.** The rule used to be "a retry enters `Creating`, so drop it there" — which was true
/// of the only way a code could ever change. It no longer is: a pin that runs out is now replaced
/// automatically, and the flow returns to the same `Waiting` it was already in. A cache keyed on
/// the phase would have gone on drawing the dead code — sharp, scannable, and pointing at a pin
/// plex.tv had forgotten — for the whole of its successor's life. `Creating` is still checked, as
/// the belt to the generation's braces: it is the one moment a flow is known to have thrown its
/// code away before any replacement exists.
///
/// The texture has to be DELETED, not merely forgotten: [`ensure_qr_tex`] allocates a fresh id on
/// every miss (`img_upload_rgba` never reuses the old one), so zeroing the handle alone orphaned a
/// full 400x400-ish RGBA QR bitmap per sign-in retry, with nothing left holding its id to free it
/// later. `gfx::delete_tex` no-ops on 0, and both callers are on the main thread (the app loop's
/// `Route::Login` arm), which is where GL deletes must happen.
fn drop_a_stale_qr(s: &mut Scene, live: u64) {
    if !qr_cache_stale(s.qr_gen, live, auth::phase()) {
        return;
    }
    crate::gfx::delete_tex(s.qr_tex);
    s.qr_tex = 0;
    s.qr_gen = live;
}

/// Whether the cached QR bitmap has stopped describing the code the flow is showing.
///
/// Pure and split out from the delete for the reason every other rule on this screen is: the
/// caller frees a GL texture, so no host test can reach it, and this is the half that decides
/// whether a dead code stays on the television.
fn qr_cache_stale(cached: u64, live: u64, phase: Phase) -> bool {
    cached != live || phase == Phase::Creating
}

/// Decode + upload Plex's QR PNG once, caching the GL texture. Main (draw) thread only.
fn ensure_qr_tex(s: &mut Scene, qr: &auth::QrCode) {
    // The SNAPSHOT's generation, not a fresh read: the texture about to be uploaded and the number
    // it is cached under must come from one lock, or a later frame keys the new bitmap by the old
    // code. Both call sites run the same rule, so a replaced code can never be drawn out of a
    // cache that `update` happened not to have reached yet this frame.
    drop_a_stale_qr(s, qr.generation);
    if s.qr_tex != 0 || qr.png.is_empty() {
        return;
    }
    let (mut w, mut h): (c_int, c_int) = (0, 0);
    let px = crate::img::img_decode_rgba(qr.png.as_ptr(), qr.png.len() as c_int, &mut w, &mut h);
    if !px.is_null() {
        s.qr_tex = crate::img::img_upload_rgba(px, w, h);
        crate::img::img_free(px);
    }
}

pub fn draw() {
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    let p = Painter::root();
    let s = scene();
    // The QR screen is the first thing a new user sees, before Home has any artwork to lend it.
    // Use the shared pre-content route ground rather than a local grey clear.
    s.ground.draw_default(p);
    let env = Env::inert();

    if auth::pending_persistence_warning().is_some() {
        let layout = RouteLayout::screen();
        layout.draw_narrative(
            p, None, "Your sign-in couldn’t be saved",
            "You’re signed in for this session, but this TV may ask you to sign in again after you close the app.",
            theme::size::LABEL,
        );
        TextView::new(
            "Open Details to send a report so we can investigate the saving problem. Choose Continue to use Plex now.",
            theme::size::BODY, theme::TEXT_SECONDARY,
        ).draw(p, layout.content);
        draw_storage_action(p, &env, layout);
    } else { match auth::phase() {
        Phase::Waiting => draw_waiting(p, &env, s),
        Phase::Error => draw_failed(p, &env, s),
        Phase::Deleted => draw_deleted(p, &env, s),
        Phase::Resetting => draw_working(p, &env, s, "Clearing local account\u{2026}"),
        Phase::Discovering => draw_working(p, &env, s, "Finding your server\u{2026}"),
        _ => draw_working(p, &env, s, "Connecting to Plex\u{2026}"),
    } }
    // The one-off report alert covers the route; its scrim includes the whole host screen.
    s.report_alert.draw_scrim();
    s.report_alert.draw(
        c"Not now",
        c"Send report",
    );
    // Details and the report confirmation are mutually exclusive presentations.
    if details_pop().visible() {
        let dp = details_pop().painter(theme::alert::SCRIM_A, Popover::RISE);
        let r = details_panel_rect();
        s.details_report_rect = storage_report_available().then(|| details_report_rect(r));
        details_pop().panel(dp, r, DETAILS_PANEL_RAD);
        let title_h = crate::text::text_height(theme::size::LABEL, 0);
        Label::new(
            DETAILS_TITLE.as_ptr(),
            theme::size::LABEL,
            theme::TEXT_HEADING,
        )
        .draw(
            dp,
            Rect::new(
                r.x + theme::space::LG,
                r.y + theme::space::MD,
                r.w - 2.0 * theme::space::LG,
                title_h,
            ),
        );
        details_table().draw(dp, details_table_rect(r));
        let report_rect = details_report_rect(r);
        let guidance_h = details_footer_h()
            - 2.0 * theme::space::SM
            - crate::ui::widgets::StatusOverlay::CTRL_H;
        TextView::new(
            DETAILS_REPORT_GUIDANCE,
            theme::size::CAPTION,
            theme::TEXT_SECONDARY,
        )
        .draw(
            dp,
            Rect::new(
                r.x + theme::space::LG,
                report_rect.y - theme::space::SM - guidance_h,
                r.w - 2.0 * theme::space::LG,
                guidance_h,
            ),
        );
        if storage_report_available() {
            Button::new(
                DETAILS_REPORT.as_ptr(),
                theme::size::BODY,
                report_rect,
            )
            .focused(true)
            .draw(&Env::inert(), dp);
        }
        let back_hint = crate::ui::widgets::KeyHint::new(c"Click", c"BACK", c"to return");
        back_hint.draw(
            dp,
            r.x + r.w - theme::space::LG - back_hint.width(),
            report_rect.y + report_rect.h * 0.5,
        );
    } else {
        s.details_report_rect = None;
    }
}

/// The three non-QR states are ONE centred read-out, not the two-column route.
///
/// **They used to be that route**, with the title and a sentence in the narrative column and a
/// lone spinner floating in the content column — which is the composition for a screen that has a
/// LIST or a document on the right, and reads as a broken one when the right-hand side holds a
/// single 26px ring. `StatusOverlay` is the app's existing answer for "the whole surface is
/// waiting": spinner over verdict over an optional reason over the one action, centred on the area
/// the wait is ABOUT — `Rect::FULL` here, since none of this screen exists yet.
fn draw_readout(
    p: Painter,
    env: &Env,
    s: &Scene,
    caption: &std::ffi::CStr,
    kind: StatusKind,
    reason: Option<&std::ffi::CStr>,
    action: Option<&'static std::ffi::CStr>,
    // The current report's delivery status, distinguishing queued from confirmed delivery.
    note: Option<&std::ffi::CStr>,
) {
    let mut o = StatusOverlay::new(Rect::FULL, caption, kind).phase(s.spin_ms as u32);
    if let Some(r) = reason {
        o = o.reason(r);
    }
    if let Some(a) = action {
        // The only control on the screen, so it holds focus by construction — there is nowhere
        // else for the ring to be, and OK must reach it without a press to move focus first.
        o = o.action(a).focused(true);
    }
    o.draw(env, p);
    let line_h = crate::text::text_height(theme::size::CAPTION, 0);
    let below = o.action_frame().map(|a| a.y + a.h);
    if let Some(n) = note {
        let y = below.map_or(o.frame.y + o.frame.h, |b| b + theme::space::SM);
        Label::new(n.as_ptr(), theme::size::CAPTION, theme::TEXT_SECONDARY)
            .h(HAlign::Center)
            .draw(p, Rect::new(o.frame.x, y, o.frame.w, line_h));
    }
}

fn draw_working(p: Painter, env: &Env, s: &Scene, msg: &str) {
    let caption = CString::new(msg).unwrap_or_default();
    let stuck = escape_ready(s);
    draw_readout(
        p,
        env,
        s,
        &caption,
        StatusKind::Working,
        // The reason arrives WITH the control, and only then: it exists to explain why a button
        // just appeared under a spinner that was doing fine a moment ago.
        stuck.then_some(c"This is taking longer than usual."),
        stuck.then_some(ESCAPE),
        None,
    );
}

fn draw_failed(p: Painter, env: &Env, s: &Scene) {
    let reason = CString::new(auth::error()).unwrap_or_default();
    draw_readout(
        p,
        env,
        s,
        c"Couldn\u{2019}t sign in",
        StatusKind::Failed,
        (!reason.is_empty()).then_some(reason.as_c_str()),
        Some(ESCAPE),
        current_report_note(s.report_send_failed),
    );
}

/// What the delete actually achieved, as the two lines it is honest to draw.
///
/// **A partial wipe may not be reported as a whole one**, and that is not pedantry: the files this
/// sweep can fail on include the TELEMETRY decision, so a survivor is re-read on the next launch
/// and a consent the user believed they had deleted comes back. The session is gone either way —
/// `auth::erase_local_state` is unconditional — so the verdict stays true and the reason carries
/// the qualification.
fn deleted_readout(leftovers: usize) -> (&'static std::ffi::CStr, &'static std::ffi::CStr) {
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

/// Record what the delete left behind, before the app routes here.
pub fn note_delete_leftovers(n: usize) {
    scene().delete_leftovers = n;
}

/// **Empty, not Failed.** Deleting everything is a completed action the user asked for, so it must
/// not wear the danger tint — the same distinction `StatusKind::Empty` carries for a library with
/// nothing in it. A partial one is still not a FAILURE either: what it did do, it did.
fn draw_deleted(p: Painter, env: &Env, s: &Scene) {
    let leftovers = s.delete_leftovers.max(usize::from(auth::reset_cleanup_warning()));
    let (verdict, reason) = deleted_readout(leftovers);
    draw_readout(
        p,
        env,
        s,
        verdict,
        StatusKind::Empty,
        Some(reason),
        Some(c"Sign in"),
        None,
    );
}

fn draw_waiting(p: Painter, env: &Env, s: &mut Scene) {
    let layout = RouteLayout::screen();
    // **Issue #76 review: a stalled key service is not a signed-out television.** When a sealed
    // sign-in is sitting on this install that this launch could not read, the narrative column
    // says so and carries the one control that can fix it, while the QR stack on the right stays
    // exactly as it is — the explicit "sign in again" choice, which is the OTHER answer and must
    // not be the only one offered. See `plex::session::LOCKED_UNAVAILABLE`.
    if crate::plex::session::secure_unavailable() {
        layout.draw_narrative(p, None, STORAGE_TITLE, STORAGE_COPY, theme::size::LABEL);
    } else if auth::reset_cleanup_warning() {
        layout.draw_narrative(
            p,
            None,
            "Signed out, with local residue",
            "Your account and reporting choices were removed. Some obsolete local files could not be cleaned up, but you can sign in again safely.",
            theme::size::LABEL,
        );
    } else {
        // Rule 11, and [`ActionRow::clear`]'s own instruction: the branch that does not draw the
        // band forgets its frame, so the pill that is no longer on screen is not hit-testable
        // where it used to be. The read-out comes and goes WITHIN one visit to this route —
        // `secure_unavailable()` flips the moment a retry opens the envelope — so `enter()`
        // resetting the scene is not what covers this.
        layout.draw_narrative(
            p,
            None,
            "Sign in to Plex",
            "Use your phone camera to scan the code, or link this television manually with the address and code shown here.",
            theme::size::LABEL,
        );
    }
    // Details is a small, always-available information control on the QR screen.  Try again is
    // added only for the retryable secure-unavailable state; the normal sign-in surface stays
    // visually quiet until the user opens the panel.
    draw_storage_action(p, env, layout);
    let right = qr_layout(layout);
    // ONE read of the code, used for the bitmap, the digits and the sentence beneath them.
    let qr = auth::qr_snapshot();

    TextView::new("plex.tv/link", theme::size::TITLE, theme::TEXT_HEADING)
        .bold()
        .h(HAlign::Center)
        .draw(p, right.url);

    // QR on a bright card (the white border is the scan quiet-zone). Plex's own PNG → we just show it.
    let card = right.card;
    p.rrect(card, 24.0, 24.0, theme::SURFACE_QR_PLATE);
    ensure_qr_tex(s, &qr);
    if s.qr_tex != 0 {
        let pad = 30.0;
        let inner = Rect::new(
            card.x + pad,
            card.y + pad,
            card.w - 2.0 * pad,
            card.h - 2.0 * pad,
        );
        // Plex's PNG is WHITE modules on a transparent ground; tint black so the modules render dark
        // on the white card (the transparent ground shows the card) → a scannable black-on-white QR.
        p.tex(s.qr_tex, inner, 0.0, theme::scrim_black(1.0));
    } else {
        Spinner::new(card.x + card.w * 0.5, card.y + card.h * 0.5, 22.0)
            .phase(s.spin_ms as u32)
            .tint(theme::scrim_black(0.5))
            .draw(env, p);
    }

    // The manual code and waiting state remain in the same right-column stack as the URL and QR.
    // Both use couch-readable type rungs; this is an alternative sign-in path, not fine print.
    if let Ok(code) = CString::new(qr.code.to_uppercase()) {
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

    let link = auth::link_state();
    let wr = 15.0;
    let wy = right.status.cy();
    // `qr_escape_ready`, not the bare clock: while the storage read-out owns OK it also owns the
    // sentence that says to press it — see `storage_readout_showing`. The phase is `Waiting` by
    // construction here (this function is the `Waiting` arm), so the two agree everywhere else.
    let status = waiting_status(
        qr.replaced,
        qr_escape_ready(s),
        auth::link_unreachable(&link),
    );
    let status_w = crate::text::text_width(status.as_ptr(), theme::size::BODY, 0);
    let sx = right.status.cx() - (wr * 2.0 + theme::space::SM + status_w) * 0.5;
    Spinner::new(sx + wr, wy, wr)
        .phase(s.spin_ms as u32)
        .tint(theme::TEXT_SECONDARY)
        .draw(env, p);
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

    // **Issue #75.** "A report was sent" — drawn once the current attempt's trouble has actually
    // left the television, under the link-health sentence in the same secondary stack.
    if let Some(note) = current_report_note(s.report_send_failed) {
        Label::new(note.as_ptr(), theme::size::CAPTION, theme::TEXT_SECONDARY)
            .h(HAlign::Center)
            .draw(p, right.note);
    }
}

/// The line under the code, which has to answer a question that only exists now that a code can be
/// replaced: *why is this not the code I was looking at*.
///
/// A pin lives fifteen minutes and is re-minted when it runs out, so somebody who walked away
/// mid-sign-in — or whose phone has just told them the OLD code was linked — comes back to
/// different digits. Saying nothing there reads as the television having lost track of itself, and
/// it is exactly the moment they need to be told to scan again. Same position, same rung, same
/// spinner: one sentence swapped for another, never a second read-out.
/// **`stalled` outranks `code_replaced`**: one of these sentences carries an ACTION, and a line
/// that explains history is worth less than the one that offers a way forward.
///
/// **`unreachable` outranks BOTH, for the same argument carried one step further (issue #75).** A
/// new code cannot help while plex.tv itself is not answering — pressing OK just mints another pin
/// nobody can poll — so once two consecutive polls have come back empty, the sentence that names
/// the real action (check this TV's connection) is worth more than either a code offer or a
/// history note, exactly as `stalled`'s own sentence already outranks `code_replaced`'s. The
/// [`auth::link_detail`] line drawn under this one carries the *why*; this one only ever needs to
/// say *what to do*.
fn waiting_status(
    code_replaced: bool,
    stalled: bool,
    unreachable: bool,
) -> &'static std::ffi::CStr {
    if unreachable {
        c"Can\u{2019}t reach Plex. Check your TV\u{2019}s internet connection."
    } else if stalled {
        c"Still waiting — choose New code to try again"
    } else if code_replaced {
        c"That code expired — scan this one"
    } else {
        c"Waiting for you to sign in…"
    }
}

/// How long a QR code may go unscanned before the screen offers to replace it on request.
///
/// **A separate, much longer clock than [`ESCAPE_AFTER_MS`], because this wait is not a stall.**
/// Twelve seconds is right for a spinner that should have finished in one; a code on screen is
/// waiting for a person to find their phone, unlock it, open a camera and tap a link, and nagging
/// them at twelve seconds would be wrong every time. A full minute of a code that has already been
/// scanned is not.
///
/// It exists because the automatic replacement below cannot cover the case the issue reported: the
/// phone says *Account linked* while our polls are being answered `Pending` or nothing at all, and
/// the person watching knows something the television does not. Waiting out the rest of a
/// fifteen-minute lease is not a recovery.
const QR_ESCAPE_AFTER_MS: f32 = 60_000.0;

/// **What the screen is waiting ON**, as the pair the clock in [`Scene::phase_ms`] is timing.
///
/// The phase alone was the whole identity while a code could only change by leaving `Waiting`. It
/// cannot be any more: a pin that runs out is replaced automatically, `Waiting → Creating →
/// Waiting`, and `update` samples once a frame — so a replacement completed between two samples
/// (a paused main loop, a long frame) is invisible, and the FRESH code inherits the dead one's
/// age. It would then offer New code about a code that had existed for a
/// millisecond. Including the generation makes the reset exact rather than probable.
fn wait_id() -> (Phase, u64) {
    (auth::phase(), auth::qr_generation())
}

/// Is what the screen is waiting on a DIFFERENT thing from what it was waiting on last frame?
///
/// Trivial, and separate anyway, because the rule it encodes is not: a new CODE restarts the clock
/// exactly as a new PHASE does, and the version that compared phases alone is the one that would
/// offer to replace a code a millisecond old.
fn wait_restarted(seen: (Phase, u64), live: (Phase, u64)) -> bool {
    seen != live
}

/// Whether the QR screen is offering its own replacement right now. Pure, and — like
/// [`escape_ready`] — the ONE predicate behind both the sentence and the key, so a control that
/// is not drawn can never be activated.
fn qr_escape_offered(phase_ms: f32) -> bool {
    phase_ms >= QR_ESCAPE_AFTER_MS
}

/// **Is the sealed-storage read-out on screen right now?** — the ONE predicate behind its copy,
/// its control and the key that activates it, so, exactly as [`escape_ready`] does for the stalled
/// spinner, a control that is not drawn can never be pressed.
///
/// It draws over `Waiting` alone: the other three phases are their own full-screen read-outs
/// (`draw_working`/`draw_failed`/`draw_deleted`), each already carrying one control of its own,
/// and a second offer stacked on those is a screen with two answers to one press.
///
/// `pub(crate)` (not just this module's own `key`) since 2026-09-10: it is also the predicate
/// `app.rs`'s `key_onboarding` uses to decide whether an OK-down on this route arms the shared
/// [`crate::ui::press`] control-face press (see [`commit_storage_retry`]) instead of falling
/// through to [`key`] — the same one-predicate-for-draw-and-key rule, now spanning the module
/// boundary the arm/commit split puts between the two halves of one press.
pub(crate) fn storage_readout_showing() -> bool {
    auth::phase() == Phase::Waiting && crate::plex::session::secure_unavailable()
}

/// The action row exists for every waiting QR screen: Details is the quiet information control,
/// while the retry pill is present only when the key service can be asked again.
pub(crate) fn storage_action_showing() -> bool {
    auth::phase() == Phase::Waiting || auth::pending_persistence_warning().is_some()
}

/// **`app.rs`'s deferred half of the storage-retry press.** [`key_onboarding`](crate::app) arms
/// `press::begin_ctl` on the OK-down (see [`storage_readout_showing`]) instead of calling
/// straight into [`start_storage_retry`], so the spring-back bounce from `press::release` is
/// actually on screen before the retry fires — the same arm/commit split every other
/// route-family action pill uses. Called from the per-frame `press::take_commit` dispatch, once,
/// which is what makes "exactly once" a property of `press` (one commit per press) rather than of
/// this function. Commit validation also rejects the arm if a modal took ownership after OK-down.
pub(crate) fn commit_storage_retry() {
    let armed = ARMED_STORAGE_ACTION.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some((action, attempt)) = armed else { return };
    if let Some(warning) = auth::pending_persistence_warning() {
        if !warning_action_valid(action, attempt, warning.key, modal_open()) { return; }
        match action {
            StorageAction::Continue(key) => { auth::acknowledge_persistence_warning(key); }
            StorageAction::Details => open_storage_details(),
            _ => {}
        }
        crate::ui::idle::invalidate();
        return;
    }
    if !armed_storage_action_valid(
        action,
        attempt,
        auth::current_attempt(),
        auth::phase() == Phase::Waiting,
        crate::plex::session::secure_unavailable(),
        qr_escape_ready(scene()),
        modal_open(),
    ) {
        return;
    }
    match action {
        StorageAction::Continue(_) => {},
        StorageAction::Details => open_storage_details(),
        StorageAction::RetrySecureOpen => start_storage_retry(),
        StorageAction::NewCode => {
            let _ = auth::restart_stalled_wait(scene().wait);
        }
    }
}

/// Pointer twin of the action-row key path.  Hit-testing uses the same shared ActionRow rects the
/// draw recorded, then moves focus before the app arms the ordinary control-face press.
pub(crate) fn action_press_at(mx: f32, my: f32) -> bool {
    if !storage_action_showing() || details_open() || scene().report_alert.visible() {
        return false;
    }
    let Some(slot) = (unsafe { (*addr_of_mut!(STORAGE_ACTION_POP)).hit(mx, my) }) else {
        return false;
    };
    if slot == STORAGE_FOCUS_RETRY && storage_primary_action().is_none() {
        return false;
    }
    STORAGE_FOCUS.store(slot, Ordering::Release);
    crate::ui::idle::invalidate();
    true
}

fn qr_escape_ready(s: &Scene) -> bool {
    auth::phase() == Phase::Waiting && qr_escape_offered(s.phase_ms)
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
    /// **Issue #75.** "A report about this sign-in was sent", directly under the status — drawn only
    /// once the current attempt's trouble has actually been sent, in the same secondary stack.
    note: Rect,
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
    // The raw curl/HTTP diagnostic used to reserve two extra lines here. It remains in reports,
    // while the ordinary QR screen shows one plain connection status.
    let note_h = theme::size::CAPTION as f32;
    let status_y = card.y + card.h + theme::space::LG + code_h + theme::space::MD;
    let note_y = status_y + status_h + theme::space::SM;
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
        status: Rect::new(layout.content.x, status_y, layout.content.w, status_h),
        note: Rect::new(
            layout.content.x,
            note_y,
            layout.content.w,
            note_h,
        ),
    }
}

pub fn key(sym: c_uint, wcode: c_uint) {
    // **Issue #76's report lane.** The Details panel claims every key while it is open OR still
    // fading, exactly like the one-off report alert below — the read-out and the sign-in flow
    // underneath must not react to a BACK/UP/DOWN aimed at the panel on top of them.
    if details_open() {
        if is_back(sym, wcode) {
            close_storage_details();
        } else if is_ok(sym)
            && details_pop().is_open()
            && details_pop().appear_settled()
            && storage_report_available()
        {
            open_storage_report_confirmation();
        } else if sym == SDLK_UP {
            details_table().move_sel(-1);
            crate::ui::idle::invalidate();
        } else if sym == SDLK_DOWN {
            details_table().move_sel(1);
            crate::ui::idle::invalidate();
        }
        return;
    }
    // **Issue #75.** The one-off report alert claims every key while it is open OR still fading
    // out — nothing reaches the read-out underneath, exactly as `ui::consent`'s delete alert does.
    // `visible()`, not `is_open()`: see [`modal_open`]'s doc for why a second BACK during the exit
    // fade must still be swallowed here rather than falling through to the root rule.
    if scene().report_alert.visible() {
        let s = scene();
        if is_back(sym, wcode) {
            s.report_prompt_storage_attempt = None;
            s.report_alert.dismiss();
        } else if sym == SDLK_LEFT {
            s.report_alert.move_focus(-1);
        } else if sym == SDLK_RIGHT {
            s.report_alert.move_focus(1);
        } else if is_ok(sym) {
            if s.report_alert.choice() == Choice::Destructive {
                submit_report_confirmation();
            } else {
                s.report_prompt_storage_attempt = None;
                s.report_alert.dismiss();
            }
        }
        return;
    }
    // BACK leaves the app in the platform handler, never replacing this unsaved session with
    // cached credentials. Only the explicitly labelled Continue acknowledges the warning.
    if auth::pending_persistence_warning().is_some() && is_back(sym, wcode) {
        return;
    }
    if auth::phase() == Phase::Deleted && is_ok(sym) {
        auth::start_login();
        return;
    }
    // **The storage read-out owns OK while it is drawn**, and it is drawn only over `Waiting`
    // (`draw_waiting`) — the same one-predicate-for-draw-and-key rule the two timed escapes below
    // follow, and the reason `qr_escape_ready` refuses while this is up: one key cannot mean both
    // "ask the key service again" and "mint a new pin", and the control the user can SEE is the
    // one it has to mean.
    //
    // **This branch is now a defensive no-op on the real key path, not a dead one to delete.**
    // Since 2026-09-10 `app.rs`'s `key_onboarding` intercepts an OK-down here BEFORE calling into
    // this function at all — it arms `press::begin_ctl` instead (see
    // [`storage_readout_showing`]'s doc), and the actual retry now fires once from
    // `press::take_commit`'s per-frame dispatch, via [`commit_storage_retry`]. Swallowing the
    // press here rather than re-triggering it is what keeps a caller that reaches this function
    // some other way (a test, or a future key source with no press arm of its own) from starting
    // a SECOND worker underneath the deferred one — `start_storage_retry`'s own compare-exchange
    // guard already refuses to stack, so this is belt-and-suspenders, not the primary gate.
    if is_ok(sym) && storage_action_showing() {
        return;
    }
    // **Issue #76's report lane.** LEFT/RIGHT walk the storage action row's two pills — the same
    // idiom `ui::consent`'s answer row uses — only while there is a second pill to walk to at all
    // (`storage_details_pill_shown`); with none, LEFT/RIGHT here would silently do nothing anyway,
    // but a boot before the integrator ever calls [`note_storage_readout`] should not even try.
    if storage_action_showing() && storage_details_pill_shown(&scene().storage_readout) {
        if sym == SDLK_LEFT {
            STORAGE_FOCUS.store(
                if storage_primary_action().is_some() { STORAGE_FOCUS_RETRY } else { STORAGE_FOCUS_DETAILS },
                Ordering::Release,
            );
            crate::ui::idle::invalidate();
            return;
        }
        if sym == SDLK_RIGHT {
            STORAGE_FOCUS.store(STORAGE_FOCUS_DETAILS, Ordering::Release);
            crate::ui::idle::invalidate();
            return;
        }
    }
    if auth::phase() == Phase::Error && is_ok(sym) {
        auth::retry();
        return;
    }
    // **The two timed escapes are ONE press, and they must be, because they share one clock.**
    // The QR screen's *New code* action (60 s) and the stalled spinner's *Try again*
    // (12 s) both hang off `phase_ms`, so a wait that leaves `Waiting` for `Discovering` between
    // the draw and the key made the first predicate false and the SECOND one true — on the old
    // code's timer, down the unguarded path. `auth::restart_stalled_wait` takes the wait this
    // screen actually timed and refuses if the flow has moved on, so the phase it lands on cannot
    // disagree with the phase that earned the control. A `false` means exactly that happened and
    // the press is swallowed; the main loop is about to route away from here anyway.
    if is_ok(sym) && (qr_escape_ready(scene()) || escape_ready(scene())) {
        // "requested", not "restarted": the press may still be refused a line later, and the
        // event log is the one place this failure is read from — a claim it did something is
        // exactly the wrong thing to have written there.
        crate::log("login: user requested a restart of a stalled sign-in");
        if auth::restart_stalled_wait(scene().wait) {
            // The restart usually re-enters the phase it just left (a stalled `Creating` starts
            // another `Creating`), and `update` only zeroes the clock when the wait's IDENTITY
            // changes — a fresh code changes it, a re-entered phase may not — so without this the
            // new attempt could inherit the dead one's age and show its way out immediately.
            scene().phase_ms = 0.0;
        }
        return;
    }
    // BACK backs out of the sign-in — but only when there is somewhere to back out TO. This screen
    // is reached two ways: a first-ever boot with no session (nothing behind it — the QR screen is
    // the whole app) and the Home account menu's "Sign in" (a working session is still on disk).
    // `auth::cancel` is the one that knows which, so it decides: it resumes the stored session and
    // the main loop routes Home, or reports false and leaves the flow running. In practice this
    // arm is reached only from a path that bypassed `app::key_onboarding`'s root rule — that rule
    // claims every BACK here first and sends a refused one to the television's Home.
    if is_back(sym, wcode) {
        auth::cancel();
    }
    // otherwise the login screen just waits — the pin poll drives the phase from a worker thread.
}

/// The phases that are genuinely WAITING ON A NETWORK CALL and can therefore stall.
///
/// **An allowlist, not "everything that is not terminal".** It was the latter for an hour, which
/// swept in `Ready`, `Profiles` and `Switching` — phases the main loop routes away from on its
/// next pass. Key input is dispatched before that pass, so an OK aimed at the escape control the
/// user could still see would have called `auth::retry` on a flow that had already SUCCEEDED,
/// replacing a completed handoff with a fresh sign-in. `Idle` is excluded for the same reason
/// from the other side: nothing is owed, so there is nothing to retry.
fn working_phase(phase: Phase) -> bool {
    matches!(phase, Phase::Creating | Phase::Discovering)
}

/// Whether the read-out is showing its way out right now.
///
/// One predicate for the draw AND the key handler, so a control that is not drawn can never be
/// activated — the rule `player_hud::transport_hidden` states for the same hazard.
fn escape_ready(s: &Scene) -> bool {
    working_phase(auth::phase()) && escape_offered(s.phase_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consts::inside_safe;

    #[test]
    fn warning_actions_require_the_current_generation_and_exclusive_input() {
        let first = auth::PersistenceWarningKey { attempt: 7, generation: 1 };
        let next = auth::PersistenceWarningKey { attempt: 7, generation: 2 };
        assert!(warning_action_valid(StorageAction::Continue(first), 7, first, false));
        assert!(!warning_action_valid(StorageAction::Continue(first), 7, next, false));
        assert!(!warning_action_valid(StorageAction::Continue(next), 7, next, true));
        assert!(!warning_action_valid(StorageAction::Details, 6, next, false));
        assert!(warning_action_valid(StorageAction::Details, 7, next, false));
        assert!(!warning_action_valid(StorageAction::NewCode, 7, next, false));
        assert!(!warning_action_valid(StorageAction::RetrySecureOpen, 7, next, false));
    }

    // ---- Issue #76's report lane: the Details panel's row assembly (owner correction, 2026-09-11:
    // no diagnostic words in the read-out/footer, a Details pill and panel instead) ----------------

    #[test]
    fn storage_readout_is_empty_until_the_integrator_says_something() {
        assert!(StorageReadout::default().is_empty());
        assert!(!StorageReadout {
            stored_sign_in: Some("secure_refused"),
            ..StorageReadout::default()
        }
        .is_empty());
        assert!(!StorageReadout {
            candidates: vec![CandidateReadout {
                category: "developer",
                outcome: "missing",
                errno: None,
            }],
            ..StorageReadout::default()
        }
        .is_empty());
        assert!(storage_details_pill_shown(&StorageReadout {
            key_service: Some("no_reply"),
            ..StorageReadout::default()
        }));
        assert!(storage_details_pill_shown(&StorageReadout::default()));
    }

    #[test]
    fn details_is_available_before_storage_facts_exist() {
        assert!(storage_details_pill_shown(&StorageReadout::default()));
    }

    #[test]
    fn waiting_storage_actions_choose_one_stable_primary_and_always_keep_details() {
        assert_eq!(storage_primary_for(false, true, true), None);
        assert_eq!(storage_primary_for(true, false, false), None);
        assert_eq!(
            storage_primary_for(true, true, false),
            Some(StoragePrimaryAction::RetrySecureOpen)
        );
        assert_eq!(
            storage_primary_for(true, false, true),
            Some(StoragePrimaryAction::NewCode)
        );
        assert_eq!(
            storage_primary_for(true, true, true),
            Some(StoragePrimaryAction::RetrySecureOpen),
            "storage recovery wins; one visible primary can never change meaning"
        );
        assert_eq!(storage_focus_for(None, STORAGE_FOCUS_RETRY), STORAGE_FOCUS_DETAILS);
        assert_eq!(
            storage_focus_for(
                Some(StoragePrimaryAction::NewCode),
                STORAGE_FOCUS_RETRY
            ),
            STORAGE_FOCUS_RETRY
        );
    }

    #[test]
    fn deferred_login_action_never_changes_identity_or_crosses_an_attempt() {
        assert!(armed_storage_action_valid(
            StorageAction::Details, 7, 7, true, false, false, false
        ));
        assert!(!armed_storage_action_valid(
            StorageAction::NewCode, 7, 8, true, false, true, false
        ));
        assert!(!armed_storage_action_valid(
            StorageAction::RetrySecureOpen, 7, 7, true, false, true, false
        ));
        assert!(!armed_storage_action_valid(
            StorageAction::NewCode, 7, 7, true, true, true, false
        ));
        assert!(!armed_storage_action_valid(
            StorageAction::Details, 7, 7, true, false, false, true
        ));
    }

    #[test]
    fn leaving_login_closes_details_and_discards_an_armed_action() {
        let _g = crate::testlock::serial();
        init();
        open_storage_details();
        *ARMED_STORAGE_ACTION.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((StorageAction::Details, 7));
        assert!(modal_open());

        leave();

        assert!(!modal_open(), "a modal owned by Login cannot outlive that route");
        assert!(
            ARMED_STORAGE_ACTION.lock().unwrap_or_else(|e| e.into_inner()).is_none(),
            "a spring-back after the route changed must have nothing left to commit"
        );
    }

    #[test]
    fn visible_details_is_the_topmost_pointer_owner() {
        let _g = crate::testlock::serial();
        init();
        open_storage_details();
        assert!(details_press_at(-1.0, -1.0));
        assert!(
            !action_press_at(-1.0, -1.0),
            "the action row underneath a modal never receives its click"
        );
        leave();
    }

    #[test]
    fn details_table_frame_matches_panel_geometry() {
        let panel = Rect::new(0.0, 0.0, DETAILS_PANEL_W, 420.0);
        let header_h = 32.0;
        let footer_h = 88.0;
        let frame = details_table_rect_with_header(panel, header_h, footer_h);
        assert_eq!(frame.y, panel.y + theme::space::MD + header_h);
        assert_eq!(
            frame.h,
            panel.h
                - header_h
                - 2.0 * theme::space::MD
                - footer_h
        );
        assert!(frame.y + frame.h <= panel.y + panel.h - footer_h);
    }

    #[test]
    fn details_report_guidance_explains_when_sending_is_useful() {
        assert!(DETAILS_REPORT_GUIDANCE.contains("sign-in keeps failing"));
        assert!(DETAILS_REPORT_GUIDANCE.contains("after a restart"));
    }

    #[test]
    fn format_candidate_value_appends_errno_only_when_there_is_one() {
        assert_eq!(
            format_candidate_value("open_failed", Some(13)),
            "open_failed (errno 13)"
        );
        assert_eq!(format_candidate_value("missing", None), "missing");
    }

    #[test]
    fn candidate_category_label_names_the_four_known_categories_and_falls_back_for_a_fifth() {
        assert_eq!(candidate_category_label("developer"), "Developer directory");
        assert_eq!(candidate_category_label("internal"), "Internal storage");
        assert_eq!(candidate_category_label("app_dir"), "App directory");
        assert_eq!(candidate_category_label("runtime"), "Runtime directory");
        assert_eq!(candidate_category_label("other"), "other directory");
        assert_eq!(candidate_category_label("future_kind"), "future_kind directory");
    }

    /// **The row set has one row per known fact, in the fixed order the panel draws them, and
    /// nothing for a fact nobody has supplied** — the whole point of a `Details` panel that is
    /// offered before every field is necessarily known (a stalled key service has no `persist_
    /// outcome` to show, and a healthy save has no `key_service` stage to show).
    #[test]
    fn build_details_rows_includes_only_the_known_facts_plus_build_and_model() {
        let empty = build_details_rows(
            &StorageReadout::default(),
            "0.6.4 \u{00B7} webOS 4.10.0",
            "k5lp",
        );
        assert_eq!(
            empty,
            vec![
                (
                    "Build".to_string(),
                    "0.6.4 \u{00B7} webOS 4.10.0".to_string()
                ),
                ("Model".to_string(), "k5lp".to_string()),
            ],
            "nothing but Build/Model when the integrator has told the screen nothing else"
        );

        let full = StorageReadout {
            stored_sign_in: Some("secure_refused"),
            persist_outcome: Some("preserved_existing_secure"),
            preserve_reason: Some("not_proven"),
            key_service: Some("no_reply"),
            key_identity: Some("anonymous"),
            candidates: vec![
                CandidateReadout {
                    category: "developer",
                    outcome: "open_failed",
                    errno: Some(13),
                },
                CandidateReadout {
                    category: "internal",
                    outcome: "missing",
                    errno: None,
                },
            ],
            ..StorageReadout::default()
        };
        let rows = build_details_rows(&full, "0.6.4 \u{00B7} webOS 4.10.0", "k5lp");
        assert_eq!(
            rows,
            vec![
                ("Stored sign-in".to_string(), "secure_refused".to_string()),
                (
                    "Last save".to_string(),
                    "preserved_existing_secure".to_string()
                ),
                ("Save reason".to_string(), "not_proven".to_string()),
                ("Key service".to_string(), "no_reply".to_string()),
                ("Key identity".to_string(), "anonymous".to_string()),
                (
                    "Developer directory".to_string(),
                    "open_failed (errno 13)".to_string()
                ),
                ("Internal storage".to_string(), "missing".to_string()),
                ("Build".to_string(), "0.6.4 \u{00B7} webOS 4.10.0".to_string()),
                ("Model".to_string(), "k5lp".to_string()),
            ]
        );
    }

    /// Diagnostic values remain complete; the widened shared table keeps decisive closed words
    /// visible instead of applying a character-count truncation guess.
    #[test]
    fn build_details_rows_keeps_a_long_candidate_summary_complete() {
        let long_outcome: &'static str = "untrusted_mode_with_an_unusually_long_wire_word_for_a_test";
        let full = StorageReadout {
            candidates: vec![CandidateReadout {
                category: "other",
                outcome: long_outcome,
                errno: Some(i32::MAX),
            }],
            ..StorageReadout::default()
        };
        let rows = build_details_rows(&full, "build", "model");
        let (label, value) = &rows[0];
        assert_eq!(label, "other directory");
        assert_eq!(value, &(long_outcome.to_string() + " (errno 2147483647)"));
    }

    /// `note_storage_readout` is the setter [`refresh_storage_readout`] drives — proven end to end
    /// through the same `scene()` state [`build_details_rows`] reads at open time.
    #[test]
    fn note_storage_readout_replaces_whatever_the_screen_held_before() {
        let _g = crate::testlock::serial();
        init();
        note_storage_readout(
            Some("secure_refused"),
            Some("write_failed"),
            None,
            Some("no_reply"),
            Some("anonymous"),
            vec![CandidateReadout {
                category: "app_dir",
                outcome: "not_regular",
                errno: None,
            }],
        );
        assert!(storage_details_pill_shown(&scene().storage_readout));
        assert_eq!(
            scene().storage_readout.stored_sign_in,
            Some("secure_refused")
        );
        assert_eq!(scene().storage_readout.candidates.len(), 1);

        // A second call REPLACES, rather than accumulating — there is only ever one launch's worth
        // of facts.
        note_storage_readout(None, None, None, None, None, Vec::new());
        assert!(storage_details_pill_shown(&scene().storage_readout));
    }

    /// **The three shapes a launch can actually be in, as the panel renders them.** Not three
    /// arbitrary fixtures: these are the three failures issue #76's field reports divide into, and
    /// the reason the panel exists is that a photograph of it has to tell them apart. They differ
    /// in what they say about the SAVE and about the candidate locations, which is exactly the
    /// distinction a bare "your saved sign-in couldn't be read" cannot carry.
    #[test]
    fn the_details_rows_tell_the_three_real_storage_shapes_apart() {
        let values = |r: &StorageReadout| -> Vec<String> {
            build_details_rows(r, "0.6.4", "k5lp")
                .into_iter()
                .map(|(label, value)| format!("{label}={value}"))
                .collect()
        };

        // 1. LOCKED-UNAVAILABLE — the reporters' own symptom. A sealed envelope is right there and
        // was accepted as one; the key service never answered, so nothing was saved this launch.
        let unavailable = StorageReadout {
            stored_sign_in: Some("secure_unavailable"),
            persist_outcome: None,
            preserve_reason: None,
            key_service: Some("no_reply"),
            key_identity: Some("anonymous"),
            candidates: vec![CandidateReadout {
                category: "developer",
                outcome: "secure",
                errno: None,
            }],
            ..StorageReadout::default()
        };
        assert_eq!(
            values(&unavailable),
            vec![
                "Stored sign-in=secure_unavailable",
                "Key service=no_reply",
                "Key identity=anonymous",
                "Developer directory=secure",
                "Build=0.6.4",
                "Model=k5lp",
            ],
            "no 'Last save' row at all: this launch never saved, and an empty row would read as \
             one that did"
        );

        // 2. SIGN-IN-NOT-PERSISTED — the fresh sign-in that was DELIBERATELY not written, because
        // a secure envelope this install has not yet earned the right to replace is present. The
        // reason word is the whole content of the finding.
        let not_persisted = StorageReadout {
            stored_sign_in: Some("secure"),
            persist_outcome: Some("preserved_existing_secure"),
            preserve_reason: Some("not_proven"),
            key_service: None,
            key_identity: Some("app_id"),
            candidates: vec![CandidateReadout {
                category: "app_dir",
                outcome: "secure",
                errno: None,
            }],
            ..StorageReadout::default()
        };
        assert_eq!(
            values(&not_persisted),
            vec![
                "Stored sign-in=secure",
                "Last save=preserved_existing_secure",
                "Save reason=not_proven",
                "Key identity=app_id",
                "App directory=secure",
                "Build=0.6.4",
                "Model=k5lp",
            ],
            "the reason rides the save's own row — two rows would read as two separate facts"
        );

        // 3. WRITE-FAILED-EVERYWHERE — no key service in the story at all. Every candidate refused
        // the write, and the per-candidate errnos are the only thing that says WHY, which is why
        // one row per location is worth the space.
        let write_failed = StorageReadout {
            stored_sign_in: Some("none"),
            persist_outcome: Some("write_failed"),
            preserve_reason: None,
            key_service: None,
            key_identity: None,
            candidates: vec![
                CandidateReadout {
                    category: "developer",
                    outcome: "open_failed",
                    errno: Some(13),
                },
                CandidateReadout {
                    category: "internal",
                    outcome: "missing",
                    errno: None,
                },
            ],
            ..StorageReadout::default()
        };
        assert_eq!(
            values(&write_failed),
            vec![
                "Stored sign-in=none",
                "Last save=write_failed",
                "Developer directory=open_failed (errno 13)",
                "Internal storage=missing",
                "Build=0.6.4",
                "Model=k5lp",
            ],
            "no key-service rows: nothing here is a verdict on a key service"
        );

        // All three offer the pill; none of them is the empty screen a boot starts on.
        for r in [&unavailable, &not_persisted, &write_failed] {
            assert!(storage_details_pill_shown(r));
        }
    }

    /// **The Details panel owns BACK and OK while it is up, and `app.rs` is told so through ONE
    /// predicate.**
    ///
    /// This route's BACK is the ROOT press — `app.rs`'s `key_onboarding` hands it to
    /// `auth::cancel` and then to the television's own Home — and it consults
    /// [`modal_open`] first precisely so a panel standing over the screen is not backed out of by
    /// backing out of the whole sign-in. The same predicate gates the OK that arms the *Try again*
    /// pill, which is still "showing" underneath an open panel.
    #[test]
    fn the_details_panel_claims_back_rather_than_the_routes_root_press() {
        let _g = crate::testlock::serial();
        init();
        assert!(!modal_open(), "the premise: nothing is open on a fresh screen");

        details_pop().open();
        assert!(
            modal_open(),
            "app.rs must route BACK here, and must not arm the pill underneath on an OK"
        );

        key(0, crate::ui::consts::WCODE_BACK);
        assert!(
            !details_pop().is_open(),
            "…and the panel's own BACK closes it rather than reaching the sign-in flow"
        );

        details_pop().close();
        assert!(!modal_open());
    }

    /// **`refresh_storage_readout` reads THIS LAUNCH's own facts** — the integrator's call, and
    /// the one that was missing: every field above is `None` and the pill is hidden on every boot
    /// until something calls it.
    #[test]
    fn refresh_storage_readout_reads_this_launchs_own_storage_facts() {
        let _g = crate::testlock::serial();
        init();
        let dir = std::env::temp_dir().join(format!("plxnative-login-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a writable temp dir");
        crate::plex::session::redirect_for_test(Some(dir.join("auth.json")));
        note_storage_readout(None, None, None, None, None, Vec::new());
        assert!(
            storage_details_pill_shown(&scene().storage_readout),
            "Details remains available before the first storage read"
        );

        // One cold load of a candidate that is not there, followed by the client-id mint's own
        // save — exactly what a first boot does.
        let _ = crate::plex::session::load();
        refresh_storage_readout();

        let r = &scene().storage_readout;
        assert!(storage_details_pill_shown(r));
        assert!(
            r.stored_sign_in.is_some(),
            "the storage class is always knowable, so this row is always there"
        );
        assert_eq!(
            r.persist_outcome,
            Some("persisted_plaintext"),
            "the mint's own save, read back through `session::last_persist_outcome`"
        );
        assert_eq!(
            r.candidates,
            vec![CandidateReadout {
                category: "other",
                outcome: "missing",
                errno: None,
            }],
            "one row per candidate this launch examined, in the read lane's own vocabulary"
        );

        crate::plex::session::redirect_for_test(None);
        note_storage_readout(None, None, None, None, None, Vec::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The read-out has to say the thing that is least guessable and most reassuring: nothing
    /// was changed.** A person looking at a QR code on a television they were already signed in to
    /// will assume they have been signed OUT — that is what this screen means everywhere else —
    /// and the whole subject here is that they have not been: the sealed sign-in is intact and the
    /// button under this copy may well open it. The other two clauses are the two ways out, and
    /// the promise that taking the second one is the only thing that replaces what is stored.
    #[test]
    fn the_storage_readout_says_the_saved_sign_in_is_still_there() {
        let says = |needle: &str| STORAGE_COPY.contains(needle);
        assert!(
            says("Nothing has been changed"),
            "the fact a stranger cannot guess, and the one that stops a needless re-sign-in"
        );
        assert!(says("key service"), "…and what actually went wrong");
        assert!(says("try again"), "the first way out — the control below this copy");
        assert!(
            says("scan the code"),
            "the second, which is the QR stack already on screen beside it"
        );
        assert!(
            says("Only signing in again replaces"),
            "…and that taking it is the only thing that costs the stored sign-in"
        );
        assert!(
            !STORAGE_TITLE.contains("keymanager"),
            "the service's bus name belongs in the log, not on the television"
        );
    }

    /// The ordinary Login screen has no technical footer; its action uses the shared route slot.
    #[test]
    fn login_root_action_uses_the_ordinary_slot_without_a_technical_footer() {
        let action = RouteLayout::screen().action;
        let y = action.y;
        assert_eq!(y, action.y, "no hidden technical footer may lift the visible action");
        assert!(
            inside_safe(Rect::new(action.x, y, action.w, action.h)),
            "…without leaving the safe area at the other end"
        );
    }

    /// **Review finding, 2026-09-10: the note's budget must derive from the room actually left,
    /// never a fixed one-line assumption.** At the measured device CAPTION line height (~30px,
    /// the same device-font scale), the pill sits inside `copy_bottom`'s `space::XL` reservation,
    /// so the room left for the note is bounded and a fixed one-line draw used to
    /// run straight past it into the body's own territory. This pins that the computed room is
    /// small (not merely "some room"), and that the max-lines budget any caller derives from it can
    /// never exceed what the room can actually hold before the note view's own bottom edge is
    /// clamped to sit inside it.
    #[test]
    fn the_retry_note_budget_derives_from_the_room_left_above_the_pill() {
        let action = RouteLayout::screen().action;
        let r = Rect::new(
            action.x,
            action.y,
            120.0,
            action.h,
        );
        let room = storage_note_room(action, r);
        // The room is the body's own bounded `space::XL` gap, not an assumed number of lines,
        // which is exactly the gap the fixed version silently ran past.
        assert!(room >= 0.0, "the room must never go negative — `storage_note_room` clamps it");
        assert!(
            room < theme::space::XL,
            "the room can never exceed the body's whole `space::XL` reservation"
        );
        // A budget derived from this room, at the CAPTION leading `draw_storage_action` uses, must
        // never claim a second line the room does not actually justify — the one-line floor below
        // is the deliberate exception: it always shows SOMETHING, even in a room too small for a
        // whole line, rather than silently drawing nothing.
        let leading = theme::size::CAPTION as f32 + theme::space::XS;
        let max_lines = ((room / leading).floor() as usize).clamp(1, 2);
        assert!(max_lines >= 1, "always show something, even with no room at all");
        if max_lines >= 2 {
            assert!(
                room >= 2.0 * leading,
                "a 2-line budget must be justified by the room actually measured, not assumed"
            );
        }
    }

    /// **Review finding: `settle_storage_retry` must not act on a worker's result once the flow it
    /// was about has moved on.** The attempt id alone cannot see a same-attempt race — a normal
    /// sign-in completion never bumps it — so the phase has to be checked too; see
    /// [`storage_retry_still_applies`]'s own doc for the two directions this covers.
    #[test]
    fn a_stale_storage_retry_result_is_dropped_once_a_newer_sign_in_has_won() {
        // The press's own moment: attempt 7, phase Waiting — exactly what `storage_readout_showing`
        // requires before the pill can even be pressed.
        assert!(
            storage_retry_still_applies(7, Phase::Waiting, 7),
            "nothing has moved on — the envelope belongs to the flow still on screen"
        );
        // The QR flow scanned on the phone finished the SAME attempt while the key-service worker
        // was still in flight — `Ctl::attempt` never moves for a normal completion, only the phase
        // does. This is the race the attempt id alone cannot see.
        assert!(
            !storage_retry_still_applies(7, Phase::Ready, 7),
            "a completed sign-in — same attempt, new phase — must win over the stale envelope"
        );
        assert!(
            !storage_retry_still_applies(7, Phase::Profiles, 7),
            "…and the picker branch of that same completion must win too"
        );
        // A fresh reset (BACK, then a retry) — different attempt, but it can land back on the same
        // phase this press started against. The phase alone cannot see THIS race.
        assert!(
            !storage_retry_still_applies(7, Phase::Waiting, 8),
            "a fresh attempt — same phase, new id — must also win over the stale envelope"
        );
        // Both moved on at once (a fresh sign-in that has already reached Ready): still refused.
        assert!(!storage_retry_still_applies(7, Phase::Ready, 9));
    }

    /// **A second press after a failed retry must ask again** (review finding, 2026-09-11). The
    /// press claimed the worker slot out of [`RETRY_IDLE`] alone, and a failed worker leaves it at
    /// [`RETRY_FAILED`] — so from the second press onward the one control on this screen did
    /// NOTHING, silently, for the rest of the visit, on the exact screen whose subject is a
    /// television that seems stuck. The two states a press may claim from are the two settled ones;
    /// the two in-flight ones ([`RETRY_BUSY`], and [`RETRY_OPENED`] waiting for the main thread to
    /// apply it) still refuse, which is the no-stacking rule that was always right.
    #[test]
    fn a_press_after_a_failed_retry_asks_again() {
        use std::sync::atomic::Ordering;
        let _g = crate::testlock::serial();
        for (from, claims, why) in [
            (RETRY_IDLE, true, "the first press on a fresh read-out"),
            (
                RETRY_FAILED,
                true,
                "a press after one that got nowhere — the service may answer this time",
            ),
            (RETRY_BUSY, false, "never while a worker is still asking"),
            (
                RETRY_OPENED,
                false,
                "nor once one has opened and is waiting for the main thread to apply it",
            ),
        ] {
            STORAGE_RETRY_STATE.store(from, Ordering::Release);
            assert_eq!(claim_storage_retry(), claims, "{why}");
            assert_eq!(
                STORAGE_RETRY_STATE.load(Ordering::Acquire),
                if claims { RETRY_BUSY } else { from },
                "a claimed press marks the slot busy; a refused one changes nothing ({why})"
            );
        }
        STORAGE_RETRY_STATE.store(RETRY_IDLE, Ordering::Release);
    }

    /// The note under the control, as the three questions it answers — and the one that matters:
    /// a second failure must not read as a dead end while a working way in is drawn beside it.
    #[test]
    fn a_second_failed_retry_still_points_at_the_way_in() {
        assert!(storage_note(RETRY_IDLE).is_none(), "a fresh read-out adds nothing");
        assert!(
            storage_note(RETRY_OPENED).is_none(),
            "an opened envelope leaves this screen entirely — there is nobody left to tell"
        );
        let busy = storage_note(RETRY_BUSY).expect("a press has to be visibly doing something");
        assert!(busy.ends_with('\u{2026}'));
        let failed = storage_note(RETRY_FAILED).expect("…and so does a press that got nowhere");
        assert!(
            failed.contains("scan"),
            "a second failure names the way in that is still open"
        );
    }

    /// **A partial wipe may not be reported as a whole one.** The sweep's candidate lists span
    /// both webOS install prefixes and the jail profiles disagree about which are writable, so a
    /// survivor is ordinary — and the survivor can be the TELEMETRY decision, which is then
    /// re-read on the next launch. Saying "telemetry has been removed" over that is the one
    /// sentence on this screen that could be actively false.
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

    /// **A stalled sign-in has to be escapable, and until 2026-09-02 it was not.** BACK on this
    /// screen goes through `auth::cancel`, which resumes a STORED session — on a first-ever boot
    /// there is none, so the key is swallowed by design and the only way out of a hung discovery
    /// was killing the app. The control appears on a clock, so the whole rule is a pure predicate.
    #[test]
    fn a_wait_that_stops_looking_normal_grows_a_way_out() {
        assert!(!escape_offered(0.0), "a fresh wait offers nothing");
        assert!(
            !escape_offered(ESCAPE_AFTER_MS - 1.0),
            "nor does a healthy one — a button that flashes past teaches people to ignore it"
        );
        assert!(escape_offered(ESCAPE_AFTER_MS));
    }

    /// **The escape belongs ONLY to the two phases that wait on a network call.** A terminal
    /// state carries its own control, and — the reason this is an allowlist rather than "not
    /// terminal" — `Ready`, `Profiles` and `Switching` are phases the main loop routes away from
    /// on its NEXT pass. Keys are dispatched before that pass, so an escape offered there could
    /// call `auth::retry` on a flow that had already succeeded and replace the handoff with a
    /// fresh sign-in.
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
            Phase::Resetting,
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
    /// half of issue #30. The cache used to be keyed on the phase — sound while the only way to
    /// get a new code was a retry, which passes through `Creating`. A pin that runs out is now
    /// re-minted automatically and the flow returns to the same `Waiting` it was already in, so a
    /// phase-keyed cache would have kept a sharp, scannable QR on screen pointing at a pin plex.tv
    /// had forgotten — for the whole of its successor's life.
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
        // the belt beside those braces: a flow that has thrown its code away has no successor yet,
        // so there is no generation to compare against, only a phase that says the QR is gone.
        assert!(qr_cache_stale(5, 5, Phase::Creating));
    }

    /// The one line under the code has to explain a swap the user did not ask for — including to
    /// somebody whose phone has just told them the OLD code was linked.
    #[test]
    fn a_swapped_code_says_so_rather_than_changing_under_the_user() {
        let says =
            |s: &std::ffi::CStr, word: &[u8]| s.to_bytes().windows(word.len()).any(|w| w == word);
        assert!(says(waiting_status(false, false, false), b"Waiting"));
        assert!(
            says(waiting_status(true, false, false), b"expired"),
            "it names what happened; a code that simply changes reads as a fault"
        );
        // …and the sentence that carries an ACTION outranks the one that carries history.
        assert!(says(waiting_status(true, true, false), b"New code"));
        assert!(says(waiting_status(false, true, false), b"New code"));
    }

    /// **`unreachable` outranks both `stalled` and `code_replaced` (issue #75).** A fresh code or a
    /// "press OK" offer are both things a working plex.tv could act on; while plex.tv itself is not
    /// answering, neither helps, so the sentence that names the real action — check this
    /// television's own connection — wins regardless of what else is true of the wait.
    #[test]
    fn unreachable_outranks_every_other_waiting_sentence() {
        let says =
            |s: &std::ffi::CStr, word: &[u8]| s.to_bytes().windows(word.len()).any(|w| w == word);
        for (code_replaced, stalled) in [(false, false), (true, false), (false, true), (true, true)]
        {
            assert_eq!(
                waiting_status(code_replaced, stalled, true),
                c"Can’t reach Plex. Check your TV’s internet connection.",
                "the QR screen projects one plain connection status, not a second diagnostic line"
            );
            assert!(
                says(
                    waiting_status(code_replaced, stalled, true),
                    b"Can\xe2\x80\x99t reach Plex"
                ),
                "code_replaced={code_replaced} stalled={stalled}: unreachable must win regardless"
            );
        }
        // …and stays silent about the link whenever plex.tv is answering, whatever else is true.
        assert!(!says(
            waiting_status(true, true, false),
            b"Can\xe2\x80\x99t reach Plex"
        ));
    }

    /// **The QR screen's clock is not the spinner's, and it must not be.**
    ///
    /// `ESCAPE_AFTER_MS` is 12 s because a discovery spinner should have finished in one. A code
    /// on screen is waiting for a person to find a phone, unlock it, open a camera and tap a link,
    /// so offering to replace it at twelve seconds would be wrong on every healthy sign-in. It is
    /// offered eventually because the automatic replacement cannot cover the reported case: the
    /// phone says *Account linked* while our polls say nothing, and waiting out the rest of a
    /// fifteen-minute lease is not a recovery.
    #[test]
    fn the_qr_screen_offers_a_new_code_on_a_much_longer_clock_than_a_stalled_spinner() {
        assert!(QR_ESCAPE_AFTER_MS > ESCAPE_AFTER_MS * 4.0);
        assert!(!qr_escape_offered(0.0));
        assert!(
            !qr_escape_offered(ESCAPE_AFTER_MS),
            "a sign-in that is merely twelve seconds old is going fine"
        );
        assert!(
            QR_ESCAPE_AFTER_MS < 900_000.0,
            "…and it must arrive well inside a code's own fifteen-minute life, or it is not a \
             recovery from anything"
        );
        assert!(qr_escape_offered(QR_ESCAPE_AFTER_MS));
    }

    /// **A new code starts a new clock, even if the phase change between them was never sampled.**
    ///
    /// `update` samples once a frame. An automatic replacement is `Waiting → Creating → Waiting`,
    /// so a long frame or a paused loop can miss the middle entirely — and a clock keyed on the
    /// phase alone would then hand the fresh code its predecessor's age and offer to replace it
    /// immediately.
    #[test]
    fn a_replaced_code_restarts_the_wait_even_when_the_phase_never_appeared_to_change() {
        let old_code = (Phase::Waiting, 7u64);
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

    #[test]
    fn qr_is_vertically_centred_and_the_whole_link_stack_stays_in_the_right_column() {
        let route = RouteLayout::screen();
        let q = qr_layout(route);
        assert_eq!(q.card.cy(), Rect::FULL.cy());
        // The report note remains inside the right-column stack without a raw diagnostic slot.
        for r in [q.url, q.card, q.code, q.status, q.note] {
            assert!(r.x >= route.content.x);
            assert!(r.x + r.w <= route.content.x + route.content.w);
            assert!(inside_safe(r));
        }
        assert!(
            q.note.y >= q.status.y + q.status.h,
            "the report receipt status must remain below the one visible connection status"
        );
    }

    // ---- issue #75: plex.tv link health on the sign-in screen ----

    /// The one thing standing between a screen that must keep animating (the link sentence counts
    /// up while plex.tv stays unreachable) and one that must stop (a healthy sign-in, where the
    /// sentence is `None` forever): comparing the drawn value, not a bare "did a poll happen" flag.
    #[test]
    fn link_detail_changed_is_silent_on_a_repeated_value_and_reports_a_real_one() {
        assert!(!link_detail_changed(&None, &None), "healthy the whole time");
        assert!(link_detail_changed(&None, &Some("x".into())), "went bad");
        assert!(
            !link_detail_changed(&Some("a".into()), &Some("a".into())),
            "same sentence redrawn is not a change"
        );
        assert!(
            link_detail_changed(&Some("a".into()), &Some("b".into())),
            "the try count or duration advanced — a real change while still unreachable"
        );
        assert!(link_detail_changed(&Some("x".into()), &None), "recovered");
    }

    // ---- issue #75: the one-off sign-in report alert ----

    /// **A fresh attempt with no trouble yet offers nothing**, whatever this screen last offered
    /// an alert for.
    #[test]
    fn no_trouble_offers_no_alert() {
        assert_eq!(offer_alert(None, None), None);
        assert_eq!(offer_alert(Some(3), None), None);
    }

    /// **A new attempt's trouble is offered exactly once**, and never again for the SAME attempt
    /// once it has been answered — dismissed or sent, `offered_for` records either the same way.
    #[test]
    fn a_trouble_is_offered_once_per_attempt() {
        assert_eq!(
            offer_alert(None, Some((5, false))),
            Some(5),
            "a fresh trouble with nothing offered yet must open"
        );
        assert_eq!(
            offer_alert(Some(5), Some((5, false))),
            None,
            "already offered (and, however it was answered) for this same attempt — must not reopen"
        );
        assert_eq!(
            offer_alert(Some(5), Some((6, false))),
            Some(6),
            "a LATER attempt's trouble is a different question and must open on its own"
        );
    }

    /// **A trouble already sent automatically (standing consent) is never offered as a one-off** —
    /// the person has nothing left to press, whatever this screen has or hasn't offered before.
    #[test]
    fn an_auto_reported_trouble_is_never_offered() {
        assert_eq!(offer_alert(None, Some((5, true))), None);
        assert_eq!(offer_alert(Some(1), Some((5, true))), None);
    }

    #[test]
    fn automatic_report_offer_waits_until_details_is_fully_gone() {
        assert!(!auto_report_offer_allowed(false, true));
        assert!(!auto_report_offer_allowed(true, false));
        assert!(auto_report_offer_allowed(false, false));
        assert_eq!(
            offer_alert(None, Some((7, false))),
            Some(7),
            "the pending trouble remains offerable after Details disappears"
        );
    }

    /// The queued caption reflects durable acceptance by the reporting lane, while a failed press
    /// with nothing queued yet gets its own caption rather than silence.
    #[test]
    fn the_sent_note_only_draws_once_something_was_actually_sent() {
        assert_eq!(report_note(false, false), None);
        assert!(report_note(true, false).is_some());
        assert_ne!(report_note(true, false), report_note(false, true));
        assert!(report_note(false, true).is_some());
        assert_eq!(
            report_note(true, true),
            report_note(true, false),
            "a sent trouble reads as sent even if some earlier press on it had failed"
        );
        let queued = report_note(true, false).unwrap().to_str().unwrap();
        assert!(queued.contains("queued"));
        assert!(!queued.contains("was sent"));
    }

    /// **REPORT_BODY keeps the complete disclosure copy.** The alert measures the real wrapped
    /// body and owns the viewport; this host test guards the bounded static copy and its decisive
    /// privacy statements without reaching SDL2_ttf.
    #[test]
    fn report_disclosure_stays_bounded_and_complete() {
        const BUDGET_CHARS: usize = 512;
        let n = REPORT_BODY.chars().count();
        assert!(
            n <= BUDGET_CHARS,
            "REPORT_BODY is {n} characters; over {BUDGET_CHARS} it needs another visual fit check — \
             shorten it, never let the privacy disclosure fall off silently"
        );
        assert!(
            REPORT_BODY.contains("random report ID identifies only this report"),
            "the disclosure must explain that the event receipt is ephemeral"
        );
        assert!(
            REPORT_BODY.contains("storage and key-service diagnostics"),
            "the disclosure must name the storage diagnostics"
        );
    }

    #[test]
    fn storage_report_status_never_claims_delivery_for_queued_or_sending() {
        assert_eq!(
            storage_report_status(Some(DeliveryState::Queued), false),
            Some("Report queued")
        );
        assert_eq!(
            storage_report_status(Some(DeliveryState::Sending), false),
            Some("Sending — keep the app open")
        );
        assert_eq!(
            storage_report_status(Some(DeliveryState::Failed), false),
            Some("Report failed — Send report to try again")
        );
    }

    #[test]
    fn failed_storage_report_keeps_the_footer_action_available() {
        assert!(storage_report_available_for(Some(DeliveryState::Failed), false));
        assert!(!storage_report_available_for(Some(DeliveryState::Queued), false));
        assert!(!storage_report_available_for(Some(DeliveryState::Sending), false));
        assert!(!storage_report_available_for(Some(DeliveryState::Sent), false));
    }

    #[test]
    fn link_detail_repaint_is_deferred_once_while_the_report_alert_is_visible() {
        assert_eq!(link_detail_repaint(true, false, false), (true, false));
        assert_eq!(link_detail_repaint(true, true, false), (false, true));
        assert_eq!(link_detail_repaint(true, true, true), (false, true));
        assert_eq!(link_detail_repaint(false, true, true), (false, true));
        assert_eq!(link_detail_repaint(false, false, true), (true, false));
        assert_eq!(link_detail_repaint(false, false, false), (false, false));
    }

    #[test]
    fn rejected_report_retry_does_not_display_the_previous_event_receipt() {
        let _guard = crate::testlock::serial();
        init();
        scene().storage_report_id = Some("a".repeat(32));
        scene().storage_report_state = Some(DeliveryState::Failed);
        record_storage_report_failure(42);
        assert!(scene().storage_report_id.is_none(), "a rejected submission has no new receipt");
        assert_eq!(scene().storage_report_attempt, Some(42));
        assert_eq!(scene().storage_report_state, Some(DeliveryState::Failed));
        assert!(storage_report_available());
        leave();
    }
}
