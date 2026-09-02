//! **The consent routes** — two independent choices, asked once, changeable forever after.
//!
//! This is the surface every other part of the telemetry work waits on: nothing may be collected
//! before it has been shown and answered, so until it existed `telemetry::consent`'s transition
//! functions had no caller and carried `#[allow(dead_code)]` saying so. It is also the screen most
//! likely to decide what people think of the whole feature, which is why the wording below is
//! argued for rather than filled in.
//!
//! # Why it looks like this
//!
//! **Two purposes, two decisions.** Crash reports and usage statistics are judged as different
//! things by the people who care about them, so first run asks them on consecutive route states
//! with equal one-press Share / Don’t Share answers. Nothing is stored until both have been
//! answered. Settings then represents the stored answers as two independent switches.
//!
//! **One route-screen composition.** First run and Settings share [`RouteLayout`]: measured
//! narrative flow on the left, the canonical [`TableView`] on the right, and one bottom action
//! slot. Privacy Policy and the exact-payload preview push to the same [`DocumentReader`] contract
//! as Legal documents; they are not alert sheets and do not introduce a second navigation model.
//!
//! **The prose is first-person and short.** WP260 permits layering: the on-screen layer needs who,
//! why, that it is optional and reversible, and where to read the rest — the exhaustive Art. 13 list
//! lives in `PRIVACY.md` behind the Legal screen. A legalistic register would be worse here than a
//! plain one, and not only tonally: a solo MIT project writing like a legal department is what reads
//! as pretending to be a company, which is the thing this audience reacts to.
//!
//! **"See exactly what's sent" renders every literal schema** — Syncthing's preview, and the reason
//! the claim below it is checkable rather than reassuring. Usage examples go through the real
//! serializer. Native and fallback examples replace random/build-specific runtime values with
//! explicit placeholders; handled playback and usage examples show representative members of
//! their closed fixed domains. Tests compare object keys with the sanitizer/schema allowlists, so
//! an added field cannot bypass this screen.
//!
//! # It is asked ONCE PER TELEVISION, before the profile picker
//!
//! **Consent here is a DEVICE decision, not a per-viewer preference**, and the storage always said
//! so: `telemetry_candidates()` is one file with no profile key, so whoever answers binds every
//! profile on the set. Until 2026-09-02 it was nevertheless asked on first arrival at Home — i.e.
//! *after* who's-watching — which put a data-protection question to whichever household member
//! happened to be picked, up to and including a managed child profile, and made a device-wide
//! answer look like a personal setting. It is now asked as soon as there is an authorized account
//! and before the picker, so the person who signed the television in is the person who answers.
//!
//! # BACK navigates; it never answers
//!
//! The two choices are consecutive route states. BACK from Product returns to Crash. **BACK from
//! Crash does nothing**, and that is the placement's one real cost: the step behind it is sign-in,
//! which cannot be undone, so there is no route to restore and the crumb above the title is
//! absent. It is not a trap — both answers are one press and refusing costs nothing — but it is
//! the reason this route is the root of its own ceremony rather than a step in the boot wizard.
//! From Settings, BACK discards the draft and Done appears only after a stored value changes.
//!
//! # It never appears on an automated boot
//!
//! [`should_show`] takes `dev::any_trigger_present()`. Getting this wrong would not fail loudly:
//! `tests/run.py` injects a token and expects Home, the fps scenes grade a heartbeat on a known
//! route, and every `sim-shot` script drives a screen it chose — a consent prompt in front of all
//! of them would quietly re-point the entire harness at a screen nobody wrote an assertion for.
use crate::telemetry::consent::{self, Consent};
use crate::ui::consts::SCR_W;
use crate::ui::decision_alert::{Choice as AlertChoice, DecisionAlert};
use crate::ui::document_reader::DocumentReader;
use crate::ui::popover::Popover;
use crate::ui::route_screen::{RouteGround, RouteLayout, RoutePush};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::theme;
use crate::ui::widgets::Button;
use crate::ui::{Env, Rect, View};
use std::ptr::{addr_of, addr_of_mut};

const SCRIM_A: f32 = theme::alert::SCRIM_A;

// ---- the words -------------------------------------------------------------------------------

/// The heading. A question rather than a notice: it is a request for a favour, which is what it
/// actually is, and the honest framing also happens to be the one that does not read as a dark
/// pattern.
const CRASH_TITLE: &str = "Share crash reports?";
const PRODUCT_TITLE: &str = "Share product analytics?";

/// Three paragraphs, in this order for a reason: who is asking, what they get, what they do not.
///
/// **"I own exactly one LG television" is the whole argument** and is literally true — it is why
/// the webOS 6 and 10 failures that reached this project cost a Cloud Test Lab slot or a reviewer's
/// patience, and why a stranger's broken set is currently invisible.
///
/// **The "not included" line is a checkable statement about the payload**, not a promise about
/// intent. An earlier draft said something closer to "I couldn't see them if I wanted to", which is
/// unverifiable and therefore worthless. This version is only sayable because usage events cannot
/// carry runtime strings and native envelopes must pass a fixed allowlist that rejects content and
/// identity scopes. Usage action fields remain fixed typed values; the only runtime strings are
/// the separately allowlisted and bounded compatibility/network dimensions shown in the preview.
const CRASH_BODY: &str = "If PlxNative crashes, it can send technical details that help find and fix the problem. Reports may include the signal, code addresses, thread information and device compatibility details. They never include titles, Plex accounts, searches, server names or addresses, tokens, subtitle text, or the product analytics identifier.";
const PRODUCT_BODY: &str = "PlxNative can share which screens and features are used and broad sign-in and playback outcomes. Reports use a random installation identifier and can include the app version, webOS version, television model and SoC, and whether a selected server is local, remote or relayed. They never include titles, Plex accounts, searches, server names or addresses, tokens, subtitle text, or exact viewing history.";

const ROW_ERRORS: &str = "Crash reports";
const ROW_ERRORS_SUB: &str = "Optional technical crash reports.";
const ROW_USAGE: &str = "Product analytics";
const ROW_USAGE_SUB: &str = "Optional feature and playback outcomes.";
const ROW_PREVIEW: &str = "See exactly what's shared";
/// First run asks about ONE purpose at a time, so its preview row says which one it will show.
const ROW_EXAMPLE: &str = "See an example report";
const ROW_POLICY: &str = "Privacy policy";
const ROW_DELETE: &str = "Delete all local data";
/// Where BACK goes from the Settings-hosted route.
const CRUMB_SETTINGS: &str = "Settings";
/// The Settings-hosted route's own title.
const SETTINGS_TITLE: &str = "Privacy & data";

/// The two answers, as the words that go on the CONTROLS.
///
/// The title directly above states which purpose is being asked about, so a button carries the
/// verb and the shortest noun that keeps the pair parallel. They read `Share Crash Reports` /
/// `Don’t Share` while they were table ROWS, because a row has no question above it to inherit
/// from and had to name the purpose itself.
fn answer_labels() -> (&'static std::ffi::CStr, &'static std::ffi::CStr) {
    if stage() == Stage::Crash {
        (c"Share reports", c"Don’t share")
    } else {
        (c"Share analytics", c"Don’t share")
    }
}

/// Row order, and the single source of it. An arm without a row, or a row without an arm, cannot
/// happen — the same shape `legal::Page::ALL` uses, and for the same reason `account_menu`'s comment
/// records: a row appended in one place and not the other is the index drift that made a menu open
/// the wrong thing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowId {
    Errors,
    Usage,
    Preview,
    Policy,
    Delete,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    FirstRun,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Crash,
    Product,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum PreviewKind {
    Payload,
    Policy,
}

fn row_ids() -> Vec<RowId> {
    if mode() == Mode::FirstRun {
        // The two ANSWERS are not rows — they are the route's action band. What is left here is
        // only what you may READ before answering.
        return vec![RowId::Preview, RowId::Policy];
    }
    vec![
        RowId::Errors,
        RowId::Usage,
        RowId::Preview,
        RowId::Policy,
        RowId::Delete,
    ]
}

// ---- state -----------------------------------------------------------------------------------

static mut POP: Popover = Popover::new();
static mut TABLE: TableView = TableView::new(); // main-thread only
/// The answer being composed. Settings mirrors the stored decision and commits it through Done;
/// first run starts empty and commits both choices only after the second question. It is never
/// written straight to `consent::CURRENT`, so a half-made choice cannot let events through.
static mut DRAFT: (bool, bool) = (false, false);
static mut BASE: (bool, bool) = (false, false);
static mut MODE: Mode = Mode::FirstRun;
static mut STAGE: Stage = Stage::Crash;
static mut PREVIEW_KIND: PreviewKind = PreviewKind::Payload;
static mut DELETE_REQUESTED: bool = false;
static mut DOCUMENT_OPEN: bool = false;
static mut DOCUMENT_MORPH: RoutePush = RoutePush::new();
static mut READER: DocumentReader = DocumentReader::new();
/// Whether the route's bottom action band holds focus — Settings' Done, or first run's two
/// answers.
static mut ACTION_FOCUSED: bool = false;
/// Which first-run answer the ring is on. A CURSOR, not a pre-selected value: the two are equals,
/// nothing times out onto either, and `draft()` is untouched until OK is actually pressed.
static mut ANSWER_SHARE: bool = true;
static mut DELETE_ALERT: DecisionAlert = DecisionAlert::new();
static mut GROUND: RouteGround = RouteGround::new();
static mut GROUND_DRAWN: bool = false;

#[allow(static_mut_refs)]
fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}
#[allow(static_mut_refs)]
fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}

pub(crate) fn is_open() -> bool {
    menu_open()
}
fn menu_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}
fn preview_open() -> bool {
    unsafe { *addr_of!(DOCUMENT_OPEN) }
}
fn reader() -> &'static mut DocumentReader {
    unsafe { &mut *addr_of_mut!(READER) }
}
fn delete_alert() -> &'static mut DecisionAlert {
    unsafe { &mut *addr_of_mut!(DELETE_ALERT) }
}

fn draft() -> (bool, bool) {
    unsafe { *addr_of!(DRAFT) }
}

fn base() -> (bool, bool) {
    unsafe { *addr_of!(BASE) }
}
/// Whether the route's bottom band holds a control at all.
///
/// First run ALWAYS does — its two answers live there, which is the whole point of the band on
/// that route. Settings grows a Done only once a value differs from the stored one.
fn action_visible() -> bool {
    match mode() {
        Mode::FirstRun => true,
        Mode::Settings => draft() != base(),
    }
}

fn mode() -> Mode {
    unsafe { *addr_of!(MODE) }
}
fn stage() -> Stage {
    unsafe { *addr_of!(STAGE) }
}
fn title() -> &'static str {
    if stage() == Stage::Crash {
        CRASH_TITLE
    } else {
        PRODUCT_TITLE
    }
}
fn body() -> &'static str {
    if stage() == Stage::Crash {
        CRASH_BODY
    } else {
        PRODUCT_BODY
    }
}

/// Should this boot put the question on screen?
///
/// Pure, and takes both inputs, so the harness rule is a host test rather than a hope — see the
/// module doc for what an automated boot landing here would do to the suite.
pub(crate) fn should_show(c: &Consent, automated: bool) -> bool {
    consent::should_ask(c, automated)
}

/// Put the device question on screen.
///
/// **Idempotent while it is already up, and that is load-bearing rather than defensive.** Since
/// the ask moved ahead of the profile picker it is made from a routing site that runs every frame,
/// and `should_show` stays true until BOTH answers are recorded — so without this the frame after
/// the first answer re-seated the stage to Crash and the second question could never be reached.
/// The guard lives here rather than at that one call site because it is a property of the
/// ceremony: nothing may restart it half-answered.
pub(crate) fn open(prev: &Consent) {
    if menu_open() && mode() == Mode::FirstRun {
        return;
    }
    unsafe {
        addr_of_mut!(MODE).write(Mode::FirstRun);
        addr_of_mut!(STAGE).write(Stage::Crash);
        addr_of_mut!(BASE).write((prev.errors, prev.usage));
        addr_of_mut!(DRAFT).write((false, false));
        DOCUMENT_OPEN = false;
        (*addr_of_mut!(DOCUMENT_MORPH)).jump(false);
        ACTION_FOCUSED = false;
        (*addr_of_mut!(GROUND)).reset();
        GROUND_DRAWN = false;
    }
    reader().reset();
    delete_alert().close();
    rebuild_initial(0);
    pop().open();
    crate::ui::idle::invalidate();
}

/// Select the second first-run purpose for a deterministic visual/performance boot.
///
/// This changes presentation state only: it neither answers nor records either choice. Keeping
/// the harness seam here means the app cannot construct a half-valid consent draft of its own.
pub(crate) fn show_product_for_dev() {
    if menu_open() && mode() == Mode::FirstRun {
        unsafe { addr_of_mut!(STAGE).write(Stage::Product) };
        rebuild(0);
        crate::ui::idle::invalidate();
    }
}

/// Open the same purposes from Settings. BACK discards the draft; Done appears only after a value
/// differs from the stored answer and is the sole commit action.
pub(crate) fn open_settings(prev: &Consent) {
    unsafe {
        addr_of_mut!(MODE).write(Mode::Settings);
        addr_of_mut!(BASE).write((prev.errors, prev.usage));
        addr_of_mut!(DRAFT).write((prev.errors, prev.usage));
        DOCUMENT_OPEN = false;
        (*addr_of_mut!(DOCUMENT_MORPH)).jump(false);
        ACTION_FOCUSED = false;
    }
    reader().reset();
    delete_alert().close();
    rebuild_initial(0);
    pop().open();
    crate::ui::idle::invalidate();
}

/// First-run consent owns an opaque frozen ground after its first draw, so the app can stop
/// repainting whatever is underneath it until the question closes.
///
/// **Only Home's update gates consult this** (`app.rs`'s Home/Library/Search arms), so it saves
/// the repaint on the already-signed-in boot that opens over Home and saves nothing on the fresh
/// sign-in, where the host is the profile picker and `profiles.rs` keeps updating under an opaque
/// question. That is a known cost rather than an oversight: the picker's own springs are cheap and
/// its roster load must not be paused by a screen in front of it.
pub(crate) fn host_ground_ready() -> bool {
    menu_open() && mode() == Mode::FirstRun && unsafe { *addr_of!(GROUND_DRAWN) }
}

/// A first-run question is a full-screen route, not a live popover over its host.  The first draw
/// may still take Home's metadata as an ambient source on the one path that opens over Home, but
/// none of Home's springs or async presentation state should advance while the question owns the
/// remote.
pub(crate) fn freezes_host() -> bool {
    menu_open() && mode() == Mode::FirstRun
}

/// Rebuild the rows against the current draft. Called on every toggle, because a `TableView` holds
/// its rows by value — the checkmark is state in the row, not a live read.
fn rebuild(sel: i32) {
    rebuild_with_motion(sel, true);
}

fn rebuild_initial(sel: i32) {
    rebuild_with_motion(sel, false);
}

fn rebuild_with_motion(sel: i32, preserve_motion: bool) {
    let (errors, usage) = draft();
    table().header_ink = theme::TEXT_READING;
    if mode() == Mode::FirstRun {
        // One headerless section holding only the two things you may READ before answering, and
        // no sub-lines: each row says all it needs in its label, and the question this screen is
        // actually asking is in the narrative column, not in this list.
        table().set_sections(
            vec![Section::new("")
                .row(
                    Row::new(if stage() == Stage::Crash {
                        ROW_EXAMPLE
                    } else {
                        ROW_PREVIEW
                    })
                    .chevron(true),
                )
                .row(Row::new(ROW_POLICY).chevron(true))],
            sel,
            preserve_motion,
        );
        // Focus opens on the ANSWERS, not on the reading list — this route exists to be answered,
        // and a stage change is an arrival at a fresh question, so it re-seats focus the same way
        // the first one did. `list_focused` is what keeps a row from being plated meanwhile.
        unsafe {
            ACTION_FOCUSED = true;
            ANSWER_SHARE = true;
        }
        table().list_focused = false;
    } else {
        let reporting = Section::new("Reporting")
            .row(Row::new(ROW_ERRORS).detail(ROW_ERRORS_SUB).toggle(errors))
            .row(Row::new(ROW_USAGE).detail(ROW_USAGE_SUB).toggle(usage));
        let info = Section::new("Information")
            .row(
                Row::new(ROW_PREVIEW)
                    .detail("Field-by-field previews for both report types.")
                    .chevron(true),
            )
            .row(
                Row::new(ROW_POLICY)
                    .detail("The complete PlxNative privacy policy for this build.")
                    .chevron(true),
            );
        let local = Section::new("On this TV").row(
            Row::new(ROW_DELETE)
                .detail("Sign out and remove PlxNative data from this TV.")
                .chevron(true),
        );
        table().set_sections(vec![reporting, info, local], sel, preserve_motion);
        // Settings opens on the LIST, and this has to be said rather than assumed: `TABLE` is a
        // crate global shared with first run, which parks it `list_focused = false` because its
        // focus lives in the answer band. Without restoring it here, opening Privacy from
        // Settings after a first run in the same process draws no focus anywhere — while UP/DOWN
        // and OK still act on the invisible selection.
        table().list_focused = true;
    }
    debug_assert_eq!(row_ids().len() as i32, table().n_rows());
}

fn apply_answer_with_mint(
    prev: &Consent,
    errors: bool,
    usage: bool,
    mint: impl FnOnce() -> Option<String>,
) -> Consent {
    let next = consent::apply(prev, errors, usage, || mint().unwrap_or_default());
    // Only usage analytics needs an install identity. Crash reports are deliberately anonymous,
    // so an errors-only answer must remain valid even if randomness is unavailable. A failed usage
    // mint, on the other hand, is not an identity: refuse that opt-in rather than inventing one
    // from a clock or a MAC.
    if next.usage && next.install_id.as_deref().unwrap_or("").is_empty() {
        consent::apply(prev, errors, false, String::new)
    } else {
        next
    }
}

/// Commit the completed first-run decisions or the explicit Settings action, then close.
fn commit() {
    let (errors, usage) = draft();
    record_answer(errors, usage);
}

/// Record one explicit answer and close. BACK never reaches this function.
fn record_answer(errors: bool, usage: bool) {
    let prev = consent::current().unwrap_or_default();
    let next = apply_answer_with_mint(&prev, errors, usage, crate::telemetry::mint_install_id);
    if usage && !next.usage {
        crate::log(
            "consent: no /dev/urandom — keeping error reporting choice and refusing only usage \
             analytics rather than inventing an identifier",
        );
    }
    crate::telemetry::record(next);
    // A decision can only make sending MORE restricted or newly possible, and both want a flush:
    // an opt-in drains anything this session queued, and a withdrawal is the moment the spool's
    // now-unconsented records get dropped — `flush_now` treats a record whose category is off as
    // acknowledged, so the purge happens on the same path rather than needing its own.
    crate::telemetry::flush_soon();
    close();
}

pub(crate) fn close() {
    unsafe {
        DOCUMENT_OPEN = false;
        ACTION_FOCUSED = false;
    }
    reader().reset();
    delete_alert().close();
    if menu_open() {
        pop().close();
    }
    crate::ui::idle::invalidate();
}

/// BACK: a preview returns to its question, Settings discards, and first run reverses the wizard.
/// No first-run BACK writes [`DRAFT`] or persisted consent.
pub(crate) fn on_back() -> bool {
    if delete_alert().is_open() {
        delete_alert().close();
        return true;
    }
    if preview_open() {
        unsafe { DOCUMENT_OPEN = false };
        crate::ui::idle::invalidate();
        return true;
    }
    if menu_open() {
        if mode() == Mode::Settings {
            close();
        } else if stage() == Stage::Product {
            unsafe { addr_of_mut!(STAGE).write(Stage::Crash) };
            rebuild(0);
            crate::ui::idle::invalidate();
        }
        // …and at Crash, BACK is SWALLOWED. There is no previous step to restore — this question
        // is the first thing after sign-in — and letting it fall through would drop an
        // unanswered television onto whatever route happens to be underneath.
        return true;
    }
    false
}

fn choose(share: bool) {
    let (e, u) = draft();
    if stage() == Stage::Crash {
        unsafe {
            addr_of_mut!(DRAFT).write((share, u));
            addr_of_mut!(STAGE).write(Stage::Product);
        }
        rebuild(0);
        crate::ui::idle::invalidate();
    } else {
        unsafe { addr_of_mut!(DRAFT).write((e, share)) };
        commit();
    }
}

/// OK: toggle a switch, open the preview, or commit.
pub(crate) fn on_ok() -> bool {
    if delete_alert().is_open() {
        if delete_alert().choice() == AlertChoice::Destructive {
            unsafe { addr_of_mut!(DELETE_REQUESTED).write(true) };
            close();
        } else {
            delete_alert().close();
        }
        return true;
    }
    if preview_open() {
        unsafe { DOCUMENT_OPEN = false };
        crate::ui::idle::invalidate();
        return true;
    }
    if !menu_open() {
        return false;
    }
    if action_visible() && unsafe { *addr_of!(ACTION_FOCUSED) } {
        match mode() {
            Mode::FirstRun => choose(unsafe { *addr_of!(ANSWER_SHARE) }),
            Mode::Settings => commit(),
        }
        return true;
    }
    let rows = row_ids();
    let sel = table().sel.clamp(0, rows.len() as i32 - 1);
    match rows[sel as usize] {
        RowId::Errors => {
            let (e, u) = draft();
            unsafe { addr_of_mut!(DRAFT).write((!e, u)) };
            rebuild(sel);
            unsafe { ACTION_FOCUSED = true };
            table().list_focused = false;
            crate::ui::idle::invalidate();
        }
        RowId::Usage => {
            let (e, u) = draft();
            unsafe { addr_of_mut!(DRAFT).write((e, !u)) };
            rebuild(sel);
            unsafe { ACTION_FOCUSED = true };
            table().list_focused = false;
            crate::ui::idle::invalidate();
        }
        RowId::Preview => {
            unsafe {
                PREVIEW_KIND = PreviewKind::Payload;
                DOCUMENT_OPEN = true;
            }
            reader().reset();
            crate::ui::idle::invalidate();
        }
        RowId::Policy => {
            unsafe {
                PREVIEW_KIND = PreviewKind::Policy;
                DOCUMENT_OPEN = true;
            }
            reader().reset();
            crate::ui::idle::invalidate();
        }
        RowId::Delete => {
            delete_alert().open();
        }
    }
    true
}

pub(crate) fn take_delete_request() -> bool {
    unsafe {
        let p = addr_of_mut!(DELETE_REQUESTED);
        let requested = p.read();
        p.write(false);
        requested
    }
}

pub(crate) fn on_updown(delta: i32) -> bool {
    if delete_alert().is_open() {
        return true;
    }
    if preview_open() {
        reader().move_by(delta);
        return true;
    }
    if menu_open() {
        // The action band is BELOW the list on both modes, so UP leaves it and DOWN off the last
        // row enters it. First run opens focused there rather than on the list, which is the only
        // asymmetry — its band is the answer, not a commit for edits made above it.
        if unsafe { *addr_of!(ACTION_FOCUSED) } {
            if delta < 0 {
                unsafe { ACTION_FOCUSED = false };
                table().list_focused = true;
                crate::ui::idle::invalidate();
            }
            return true;
        }
        if action_visible() && delta > 0 && table().sel == table().n_rows() - 1 {
            unsafe { ACTION_FOCUSED = true };
            table().list_focused = false;
            crate::ui::idle::invalidate();
            return true;
        }
        table().move_sel(delta);
        crate::ui::idle::invalidate();
        return true;
    }
    false
}

pub(crate) fn on_left_right(delta: i32) -> bool {
    if delete_alert().is_open() {
        delete_alert().move_focus(delta);
        return true;
    }
    // First run's two answers are a ROW, so LEFT/RIGHT is their whole navigation — the same
    // grammar the delete alert above uses, and the reason the answers had to leave the list: a
    // column of rows can only be walked with UP/DOWN, which put them in the same gesture as the
    // two documents beneath them.
    if menu_open()
        && !preview_open()
        && mode() == Mode::FirstRun
        && unsafe { *addr_of!(ACTION_FOCUSED) }
    {
        let share = delta < 0;
        if unsafe { *addr_of!(ANSWER_SHARE) } != share {
            unsafe { ANSWER_SHARE = share };
            crate::ui::idle::invalidate();
        }
        return true;
    }
    false
}

pub(crate) fn update(dt: f32) {
    pop().update(dt);
    delete_alert().update(dt);
    unsafe {
        (*addr_of_mut!(DOCUMENT_MORPH)).update(DOCUMENT_OPEN, dt);
    }
    reader().update(dt);
    // `sel` changes immediately so the focused row's ink can change in the same frame; the white
    // plate is a pair of springs and only advances here. Omitting this made Privacy the lone
    // Settings child whose text moved while its plate stayed where the screen opened.
    let layout = RouteLayout::screen();
    let table_frame = if mode() == Mode::Settings {
        layout.sectioned_table()
    } else {
        layout.content
    };
    table().update(dt, table_frame.h);
}

// ---- the payload preview ---------------------------------------------------------------------

/// **The exact schemas that may be sent**, built through their real body serialisers and sanitizer.
///
/// Not a mock-up, and that is the entire value: a hand-written sample drifts from the code the
/// moment anybody adds a field, and then the screen that exists to make the claim checkable is
/// itself a claim nobody checks. This runs `posthog::preview` over every
/// [`DiagEvent`](crate::diag::schema::DiagEvent) the build can emit, and the native preview is
/// tested against the native sanitizer's field allowlist. A schema change therefore appears here,
/// in front of the person being asked to consent to it.
///
/// The identifier shown is always a placeholder, never the stored value. A new identifier is
/// minted only when product analytics is enabled; error-only consent creates none.
pub(crate) fn preview() -> String {
    use crate::diag::schema::DiagEvent;
    let mut out = String::from(
        "Every schema this app can send. Random and build-specific values are placeholders; \
         fixed classes below are representative values from the closed domains in the Privacy \
         notice. Nothing else is sent. The usage identifier is random and is created only when \
         product analytics is enabled.\n\n",
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
    out.push_str("\n\nUsage events (only when usage reporting is on):\n");
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

fn privacy_policy() -> &'static str {
    "RESPONSIBLE FOR PLXNATIVE DATA\n\nGleb Linnik is responsible only for data PlxNative stores locally and for optional reports you choose to share.\n\nPLEX SERVICES\n\nPlxNative is an independent client for Plex. To sign you in, discover servers and provide Plex account features, the app communicates directly with Plex services. Plex processes information received by those services under Plex’s own Privacy Policy. PlxNative’s developer does not receive that information.\n\nPlex Privacy Policy: https://www.plex.tv/about/privacy-legal/\n\nPLEX MEDIA SERVERS\n\nTo browse and play media, update watch progress and use server features, PlxNative communicates directly with the Plex Media Servers you select. Those requests are handled by the selected server and its operator. PlxNative’s developer does not receive them.\n\nOPTIONAL REPORTING\n\nCrash reports and product analytics are independent, optional and reversible in Settings. Crash reports go to Sentry in Germany. Product analytics go to PostHog in Germany and use a random installation identifier.\n\nNEVER INCLUDED\n\nTitles, Plex accounts, searches, server names or addresses, tokens, subtitle text and exact viewing history are not included.\n\nCONTACT\n\nglinnik21@gmail.com"
}

// ---- draw ------------------------------------------------------------------------------------

pub(crate) fn draw_scrim() {
    // Settings is a modal over its frozen ambient host. First run is a route surface of its own,
    // like Shared Sources, and paints an opaque frozen route ground instead of dimming a page.
    if menu_open() && mode() == Mode::Settings {
        pop().scrim(SCRIM_A);
    }
}

pub(crate) fn draw() {
    if !menu_open() {
        return;
    }
    draw_question();
    // This alert is sourced from the completed Privacy route, so its scrim belongs immediately
    // before it here rather than in the host-page closure (which sits under Settings' opaque wash).
    delete_alert().draw_scrim();
    delete_alert().draw(c"Delete all local data?", c"Cancel", c"Delete");
}

/// Where BACK goes from this route, named on the crumb above the title — `None` where BACK goes
/// nowhere.
///
/// Derived rather than passed in because every entrance to this screen is known here: the Settings
/// root, and — for the second question — the first one.
fn crumb() -> Option<&'static str> {
    match (mode(), stage()) {
        (Mode::Settings, _) => Some(CRUMB_SETTINGS),
        // The ROOT of the ceremony: sign-in is behind it and cannot be undone, so there is
        // nothing honest to name. See the module doc.
        (Mode::FirstRun, Stage::Crash) => None,
        (Mode::FirstRun, Stage::Product) => Some(CRASH_TITLE),
    }
}

/// The route's bottom action band.
///
/// **First run holds its two ANSWERS here, and that is a move OUT of the list.** They were rows
/// once, which cost three things at once: a one-press decision read as a menu of four items; the
/// two answers sat in the same column, the same type and the same gesture as two documents you
/// may read first, so the difference between *deciding* and *reading* was carried by row order
/// alone; and the band underneath them spent its whole 60px saying `Press [BACK] to return` while
/// the actual choices scrolled in a list. The crumb above the title says where BACK goes now, so
/// the band is free for the thing this screen exists to collect.
///
/// Settings keeps Done, and still only once a value differs from the stored one.
fn draw_action_row(p: crate::ui::Painter, layout: RouteLayout) {
    if !action_visible() {
        return;
    }
    if mode() == Mode::Settings {
        let w = Button::pill_w(c"Done".as_ptr(), theme::size::BODY, false).min(layout.action.w);
        Button::new(
            c"Done".as_ptr(),
            theme::size::BODY,
            Rect::new(layout.action.x, layout.action.y, w, layout.action.h),
        )
        .focused(unsafe { *addr_of!(ACTION_FOCUSED) })
        .palette(crate::ui::settings::control_palette())
        .draw(&Env::inert(), p);
        return;
    }
    // Two EQUALS, so neither is measured to the other's width and neither wears a danger face:
    // declining is an ordinary answer here, not a destructive one.
    let (share, decline) = answer_labels();
    let (share_r, decline_r) = layout.action_pair(
        Button::pill_w(share.as_ptr(), theme::size::BODY, false),
        Button::pill_w(decline.as_ptr(), theme::size::BODY, false),
    );
    let focused = unsafe { *addr_of!(ACTION_FOCUSED) };
    let on_share = unsafe { *addr_of!(ANSWER_SHARE) };
    let palette = unsafe { (*addr_of!(GROUND)).palette() };
    for (label, rect, is_share) in [(share, share_r, true), (decline, decline_r, false)] {
        Button::new(label.as_ptr(), theme::size::BODY, rect)
            .focused(focused && on_share == is_share)
            .palette(palette)
            .draw(&Env::inert(), p);
    }
}

/// Draw the current route state and, when pushed, its shared document reader.
fn draw_question() {
    let p0 = pop();
    let a = p0.appear();
    if mode() == Mode::FirstRun {
        unsafe {
            // Since the ask moved ahead of the profile picker, the usual first-run host is the
            // PICKER and no hub has been fetched — so `draw_home` finds no hero metadata and the
            // authored `ROUTE_GROUND_FALLBACK` is the ground rather than a fallback. The keyed
            // hero arm is still reachable, but only on the one path that opens over a painted
            // Home: an already-signed-in boot whose policy version was bumped. Either way, never
            // sample the framebuffer — on the picker path it would freeze the app's grey clear.
            (*addr_of_mut!(GROUND)).draw_home(crate::ui::Painter::root());
            GROUND_DRAWN = true;
        }
    }
    let ground = if mode() == Mode::FirstRun {
        crate::ui::Painter::root()
    } else {
        p0.content_painter(0.0)
    };
    let entrance = ground.alpha(a).translate(SCR_W as f32 * (1.0 - a), 0.0);
    let t = unsafe { (*addr_of!(DOCUMENT_MORPH)).amount() };
    let p = unsafe { (*addr_of!(DOCUMENT_MORPH)).parent(entrance) };
    let layout = RouteLayout::screen();
    let copy = if mode() == Mode::Settings {
        "Control optional reporting, review exactly what may be shared, and manage data stored by PlxNative on this television."
    } else {
        body()
    };
    layout.draw_narrative(
        p,
        crumb(),
        if mode() == Mode::Settings {
            SETTINGS_TITLE
        } else {
            title()
        },
        copy,
        if mode() == Mode::Settings {
            theme::size::LABEL
        } else {
            theme::size::BODY
        },
    );
    let table_frame = if mode() == Mode::Settings {
        layout.sectioned_table()
    } else {
        layout.content
    };
    table().draw(p, table_frame);
    if !preview_open() {
        draw_action_row(p, layout);
    }

    if t > 0.01 {
        let policy = unsafe { *addr_of!(PREVIEW_KIND) == PreviewKind::Policy };
        let dp = unsafe { (*addr_of!(DOCUMENT_MORPH)).child(entrance) };
        // A pushed document's crumb names the question it was opened from, so the way back out of
        // a policy read is stated even on the route that has no BACK hint anywhere.
        layout.draw_narrative(
            dp,
            Some(if mode() == Mode::Settings {
                SETTINGS_TITLE
            } else {
                title()
            }),
            if policy { ROW_POLICY } else { ROW_PREVIEW },
            if policy {
                "How PlxNative handles local data, Plex services and optional reporting."
            } else {
                "The exact fields this build can send when each optional category is enabled."
            },
            theme::size::LABEL,
        );
        let text = if policy {
            privacy_policy().to_string()
        } else {
            preview()
        };
        reader().draw(dp, layout.content, None, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **An automated boot is never asked.** The failure is silent: `tests/run.py` injects a token
    /// and expects Home, the fps scenes grade a heartbeat on a known route, and every `sim-shot`
    /// script drives a screen it chose.
    #[test]
    fn an_automated_boot_never_sees_the_question() {
        assert!(!should_show(&Consent::default(), true));
        assert!(
            should_show(&Consent::default(), false),
            "…but an ordinary first boot does"
        );
    }

    /// An answer against the current policy is not re-asked, whichever way it went. Re-asking the
    /// same decided question is the nagging pattern the whole design avoids; a schema expansion
    /// deliberately has a different policy version.
    #[test]
    fn a_current_policy_answer_is_not_asked_again() {
        for (e, u) in [(false, false), (true, false), (false, true), (true, true)] {
            let answered = consent::apply(&Consent::default(), e, u, || "id".into());
            assert!(
                !should_show(&answered, false),
                "re-asked after errors={e} usage={u}"
            );
        }
    }

    /// A material schema expansion must receive two new explicit answers. The previous choices
    /// remain fail-closed while the first-run route asks the expanded questions again.
    #[test]
    fn a_policy_bump_reasks_without_reusing_the_old_answer() {
        let old = Consent {
            asked_version: consent::POLICY_VERSION - 1,
            errors: true,
            usage: true,
            install_id: Some("old-id".into()),
        };
        assert!(should_show(&old, false));
        let current = consent::apply(&Consent::default(), true, false, || "new-id".into());
        assert!(!should_show(&current, false));
    }

    #[test]
    fn unavailable_randomness_refuses_only_usage_and_keeps_error_reporting() {
        let answer = apply_answer_with_mint(&Consent::default(), true, true, || None);
        assert!(answer.answered());
        assert!(
            answer.errors,
            "anonymous error reporting does not need an install id"
        );
        assert!(
            !answer.usage,
            "usage reporting cannot start without its random id"
        );
        assert!(answer.install_id.is_none());
    }

    /// Regression for the TV report: the row selection changed its ink, but this screen never
    /// advanced the TableView springs, so the pill stayed behind until another action snapped it.
    #[test]
    fn the_consent_update_advances_the_shared_table_focus_pill() {
        let _serial = crate::testlock::serial();
        unsafe { addr_of_mut!(DRAFT).write((true, true)) };
        rebuild_initial(0);
        table().move_sel(1);
        pop().open();
        let before = table().highlight_motion();
        update(1.0 / 60.0);
        let after = table().highlight_motion();
        close();
        assert!(
            after.0 > before.0,
            "consent::update left the pill behind: {before:?} -> {after:?}"
        );
        assert!(
            after.1.abs() > 0.0,
            "the screen must expose the running spring: {after:?}"
        );
    }

    /// Rebuilding the rows after flipping On/Off changes only their values. It must not turn an
    /// in-flight focus spring into a teleport.
    #[test]
    fn toggling_a_value_preserves_in_flight_focus_motion() {
        let _serial = crate::testlock::serial();
        unsafe { addr_of_mut!(DRAFT).write((true, true)) };
        rebuild_initial(0);
        table().move_sel(1);
        let visible = table().measured_height();
        table().update(1.0 / 60.0, visible);
        let moving = table().highlight_motion();
        rebuild(1);
        assert_eq!(
            table().highlight_motion(),
            moving,
            "a value-only rebuild snapped the pill"
        );
    }

    /// **The preview is the real payload.** Every event the build can emit appears in it, built
    /// through the same serialiser a send would use — so an event added without being declared
    /// shows up in front of the person being asked to consent to it, rather than in a dashboard.
    #[test]
    fn the_preview_shows_every_event_this_build_can_emit() {
        let text = preview();
        for s in crate::diag::schema::EVENT_SPECS {
            assert!(
                text.contains(s.name),
                "the payload preview does not show `{}`",
                s.name
            );
            // …and every FIELD, which the name check alone would miss: a field added to an event
            // that is already previewed changes what a person is consenting to while the screen
            // still shows the old shape.
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
            assert!(
                text.contains(f.key),
                "the payload preview does not show context field `{}`",
                f.key
            );
        }
        for crash_field in [
            "stacktrace",
            "registers",
            "threads",
            "debug_meta",
            "image_size",
        ] {
            assert!(
                text.contains(crash_field),
                "the native crash schema omits `{crash_field}`"
            );
        }
        for fallback_field in [
            "C fault fallback",
            "Rust panic fallback",
            "fingerprint",
            "culprit",
        ] {
            assert!(
                text.contains(fallback_field),
                "the fallback schema omits `{fallback_field}`"
            );
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
            assert!(
                text.contains(handled_field),
                "the handled playback-error schema omits `{handled_field}`"
            );
        }
    }

    /// …and it shows the anonymity flag, which is the one property in the payload a reader could not
    /// otherwise verify and the one that costs a person a profile if it is ever dropped.
    #[test]
    fn the_preview_shows_the_anonymity_flag() {
        assert!(preview().contains("$process_person_profile"));
        assert!(preview().contains("false"));
    }

    /// **The preview carries no real identifier.** Settings may already hold one, while first run
    /// cannot mint one before the usage answer; neither route may expose the stored value here.
    #[test]
    fn the_preview_cannot_contain_a_real_identifier() {
        let text = preview();
        assert!(
            text.contains("created only when product analytics is enabled"),
            "the intro explains the placeholder"
        );
        assert!(
            text.contains("<random id>"),
            "and the field itself is a placeholder"
        );
        // 32 lowercase hex in a row is what a minted id looks like; nothing here may match it.
        let bytes: Vec<char> = text.chars().collect();
        let run = bytes.windows(32).any(|w| {
            w.iter()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        });
        assert!(
            !run,
            "the preview contains something shaped like a real install id"
        );
    }

    /// The row table and the id list cannot drift: a row added to one and not the other is the
    /// index bug that makes a menu act on the wrong line.
    #[test]
    fn every_row_id_has_a_row() {
        // `DRAFT` and `TABLE` are crate globals, so this takes the shared lock rather than a local
        // one — `[[test-suite-global-pollution]]`'s rule, and the reason the whole `auth` block
        // once aborted under load.
        let _g = crate::testlock::serial();
        unsafe {
            addr_of_mut!(MODE).write(Mode::FirstRun);
            addr_of_mut!(BASE).write((false, false));
            addr_of_mut!(DRAFT).write((false, false));
        }
        rebuild(0);
        assert_eq!(table().n_rows(), row_ids().len() as i32);
    }

    #[test]
    fn done_is_a_route_action_after_a_change_not_a_table_row() {
        let _g = crate::testlock::serial();
        let stored = Consent {
            errors: false,
            usage: false,
            ..Default::default()
        };
        open_settings(&stored);
        assert!(!action_visible());
        unsafe { addr_of_mut!(DRAFT).write((true, false)) };
        rebuild(0);
        assert!(action_visible());
        assert_eq!(row_ids().len(), 5, "Done never changes table geometry");
        close();
    }

    /// **The two answers are the ACTION ROW, and the list is only what you may read first.**
    /// Pinned because the failure is silent and cosmetic-looking: put an answer back among the
    /// rows and the screen still works, it just stops distinguishing deciding from reading.
    #[test]
    fn first_run_answers_are_the_action_row_and_not_table_rows() {
        let _g = crate::testlock::serial();
        open(&Consent::default());
        assert_eq!(
            row_ids(),
            vec![RowId::Preview, RowId::Policy],
            "only the two readable documents remain in the list"
        );
        assert_eq!(table().n_rows(), 2);
        assert!(
            action_visible(),
            "first run always carries its answers in the band"
        );
        assert!(
            unsafe { *addr_of!(ACTION_FOCUSED) },
            "focus opens on the answers, not on the reading list"
        );
        assert!(!table().list_focused, "so no row is plated meanwhile");
        close();
    }

    /// LEFT/RIGHT is the answer row's whole navigation, and neither direction ANSWERS: the draft
    /// is untouched until OK. That is the same separation BACK has, one gesture over.
    #[test]
    fn moving_between_the_two_answers_decides_nothing() {
        let _g = crate::testlock::serial();
        open(&Consent::default());
        assert!(unsafe { *addr_of!(ANSWER_SHARE) }, "opens on Share");
        assert!(on_left_right(1));
        assert!(!unsafe { *addr_of!(ANSWER_SHARE) });
        assert!(on_left_right(-1));
        assert!(unsafe { *addr_of!(ANSWER_SHARE) });
        assert_eq!(draft(), (false, false), "walking the row records nothing");
        assert_eq!(stage(), Stage::Crash, "and advances nothing");
        close();
    }

    /// OK on the focused answer is what actually answers — and a stage change re-seats focus on
    /// the new question's own answers rather than leaving it wherever the last one ended.
    #[test]
    fn ok_on_an_answer_records_it_and_the_next_question_reopens_on_its_own_row() {
        let _g = crate::testlock::serial();
        open(&Consent::default());
        on_left_right(1); // Don't share
        on_ok();
        assert_eq!(stage(), Stage::Product);
        assert_eq!(draft(), (false, false), "crash reports declined");
        assert!(unsafe { *addr_of!(ACTION_FOCUSED) });
        assert!(
            unsafe { *addr_of!(ANSWER_SHARE) },
            "the second question opens on Share like the first, not on the last answer given"
        );
        close();
    }

    /// UP leaves the band for the list and DOWN off the last row comes back — one vertical
    /// relationship, shared by both modes, so the band is never a dead end.
    #[test]
    fn the_answer_row_and_the_reading_list_are_one_vertical_walk() {
        let _g = crate::testlock::serial();
        open(&Consent::default());
        assert!(on_updown(-1));
        assert!(!unsafe { *addr_of!(ACTION_FOCUSED) });
        assert!(table().list_focused);
        table().sel = table().n_rows() - 1;
        assert!(on_updown(1));
        assert!(
            unsafe { *addr_of!(ACTION_FOCUSED) },
            "DOWN off the last row returns to the answers"
        );
        close();
    }

    /// **Settings opens on its list, whatever first run left behind.** `TABLE` is a crate global
    /// and first run parks `list_focused` false (its focus is the answer band), so a Settings open
    /// later in the same process inherited that and drew focus on nothing at all — while the keys
    /// still moved and committed an invisible selection.
    #[test]
    fn settings_opens_on_the_list_after_a_first_run_left_focus_in_the_answer_band() {
        let _g = crate::testlock::serial();
        open(&Consent::default());
        assert!(!table().list_focused, "first run parks it here");
        close();
        open_settings(&Consent::default());
        assert!(
            table().list_focused,
            "…and Settings has to put it back, or its focus is invisible"
        );
        assert!(!unsafe { *addr_of!(ACTION_FOCUSED) }, "and not on a Done that is not there yet");
        close();
    }

    /// **Opening an already-open question must not restart it.** The device question is asked
    /// from a routing site that runs EVERY FRAME while the profile picker is up — `should_show`
    /// stays true until BOTH answers are recorded, so an unguarded `open` re-seated the stage to
    /// Crash on the frame after the first answer and the second question was unreachable. The
    /// three call sites this replaced were all one-shot arrivals at Home, which is why the
    /// re-entrancy never mattered before.
    #[test]
    fn reopening_a_live_question_does_not_restart_the_ceremony() {
        let _g = crate::testlock::serial();
        open(&Consent::default());
        choose(true);
        assert_eq!(stage(), Stage::Product);
        open(&Consent::default());
        assert_eq!(
            stage(),
            Stage::Product,
            "the next frame's ask is a no-op, not a reset to the first question"
        );
        assert_eq!(draft(), (true, false), "and it does not discard the answer");
        close();
        open(&Consent::default());
        assert_eq!(stage(), Stage::Crash, "a genuinely fresh open still starts over");
        close();
    }

    /// Every consent route names where BACK goes — except the one where BACK goes nowhere, and
    /// that exception is the whole placement argument rather than an oversight. The device
    /// question sits immediately after sign-in, so there is no earlier step to name.
    #[test]
    fn each_route_names_where_back_goes_and_the_device_question_names_nothing() {
        let _g = crate::testlock::serial();
        open(&Consent::default());
        assert_eq!(crumb(), None, "the first question is the root of the ceremony");
        choose(true);
        assert_eq!(
            crumb(),
            Some(CRASH_TITLE),
            "the second question returns to the first"
        );
        open_settings(&Consent::default());
        assert_eq!(crumb(), Some(CRUMB_SETTINGS));
        close();
    }

    /// The two labels say which switch is which without either sounding like the safe one. Pinned
    /// because the wording IS the compliance surface: granularity means a person can tell the two
    /// purposes apart.
    #[test]
    fn the_two_switches_name_two_different_purposes() {
        assert_ne!(ROW_ERRORS, ROW_USAGE);
        assert_ne!(ROW_ERRORS_SUB, ROW_USAGE_SUB);
        assert!(privacy_policy().contains("Sentry"));
        assert!(privacy_policy().contains("PostHog"));
    }

    /// The prose carries the four things WP260's first layer needs — who, why, that it is optional,
    /// and where the rest is — plus the checkable payload claim. Asserted rather than eyeballed
    /// because a later edit for length is exactly how one of them goes missing.
    #[test]
    fn first_run_separates_crash_and_product_consent() {
        assert!(CRASH_BODY.contains("signal"));
        assert!(CRASH_BODY.contains("product analytics identifier"));
        assert!(PRODUCT_BODY.contains("random installation identifier"));
        assert!(PRODUCT_BODY.contains("exact viewing history"));
        assert_ne!(CRASH_TITLE, PRODUCT_TITLE);
    }

    /// BACK walks Product → Crash and then stops. **Stopping is the point**: the step behind the
    /// first question is sign-in, which cannot be undone, so a BACK that fell through would drop
    /// an unanswered television onto whatever route sat underneath — the profile picker it was
    /// deliberately drawn in front of. Neither press records a refusal.
    #[test]
    fn back_reverses_the_stages_and_then_holds_without_recording_a_refusal() {
        let _g = crate::testlock::serial();
        open(&Consent::default());
        choose(true);
        assert_eq!(stage(), Stage::Product);
        assert_eq!(draft(), (true, false));
        assert!(on_back());
        assert_eq!(stage(), Stage::Crash);
        assert_eq!(draft(), (true, false), "BACK does not edit either choice");
        assert!(on_back(), "and at the root it is swallowed, not passed on");
        assert!(menu_open(), "so the question is still the thing on screen");
        assert_eq!(stage(), Stage::Crash);
        assert_eq!(draft(), (true, false), "still recording nothing");
        close();
    }
}
