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
//! **The prose is first-person and short.** WP260 permits layering: this screen needs who, why,
//! that it is optional and reversible, and where to read the rest — and the rest is one push away
//! on the SAME screen, `ui::legal`'s `PRIVACY`, which carries the full Art. 13 narrative including
//! retention, rights, transfers and what uninstalling does. `PRIVACY.md` is the same narrative as a
//! repo file, plus the generated event/field schema tables; it is NOT what any screen serves and is
//! not reachable from one. This paragraph used to say the exhaustive list lived in that file
//! "behind the Legal screen", which pointed a contributor at the surface a television owner and a
//! store reviewer never see — the two must be updated together, and the screen is the one that
//! must not be allowed to fall behind. A legalistic register would be worse here than a
//! plain one, and not only tonally: a solo MIT project writing like a legal department is what reads
//! as pretending to be a company, which is the thing this audience reacts to.
//!
//! **"What is actually sent" renders every literal schema** — Syncthing's preview, and the reason
//! the claim below it is checkable rather than reassuring. Usage examples go through the real
//! serializer. Native and fallback examples replace random/build-specific runtime values with
//! explicit placeholders; handled playback and usage examples show representative members of
//! their closed fixed domains. Tests compare object keys with the sanitizer/schema allowlists, so
//! an added field cannot bypass this screen.
//!
//! **Item 14: it is TWO documents, one per telemetry channel, not one.** `preview_crash` and
//! `preview_usage` each build and render only their own channel's schemas — Sentry's crash/error
//! envelopes, or PostHog's usage events — so a person reading "what crash reports send" is never
//! shown a usage identifier or event, and the reverse. Settings shows both rows; first run shows
//! only the one the current question is actually asking about. The two functions never run
//! together except inside a test's own `preview()` union, which nothing on screen ever shows.
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
/// **Item 14: one row per CHANNEL, not one row for both.** A single combined preview left it
/// unclear which report actually carried which field, so Settings now opens two independent
/// documents — one that shows only what a crash/error report can send, one that shows only what a
/// usage/analytics event can send — each built by its own function (`preview_crash`/
/// `preview_usage`) so the split is real rather than cosmetic. See this module's doc for why the
/// preview exists at all.
/// Each channel document's title **and the label of the row that opens it** — ONE constant, not a
/// matched pair, so a row and its document cannot come to disagree about the name of the thing
/// being opened. `ui::legal`'s index is built the same way (`Row::new(page.title())`).
///
/// They WERE a pair, and the row half carried a "— what is actually sent" suffix. It earned
/// nothing: the row's own detail line already says what the row does ("Field-by-field preview of
/// …"), each document's subtitle already opens with those same four words where it has the room to
/// be a sentence, and at the row rung the suffix made these two the longest strings on the screen
/// — in a table whose other rows are two or three words. The elision it risked was already known
/// here; this comment used to state it as the reason the DOCUMENT titles stay short, which fixed
/// the shorter surface and left the longer one.
const DOC_TITLE_CRASH: &str = "Crashes / Errors";
const DOC_TITLE_USAGE: &str = "Analytics / Usage";
/// First run asks about ONE purpose at a time, so its preview row says which one it will show —
/// the click still opens the CHANNEL-scoped document that matches the current [`Stage`].
const ROW_EXAMPLE: &str = "See an example report";
const ROW_POLICY: &str = "Privacy policy";
/// The privacy contact — `ui::legal`'s constant, not a second copy. That module owns the legal
/// documents and the address they print, and its
/// `every_document_prints_only_the_one_contact_address` is what keeps them honest; a private copy
/// here would sit outside that scan.
use crate::ui::legal::CONTACT_EMAIL;
/// The row that shows the PostHog installation identifier, and the title of the document it opens.
/// It exists so that "write to us and ask for your analytics to be deleted" is an instruction a
/// person can actually follow: the identifier is the only handle those events have, and before this
/// row it was minted, persisted and sent while being visible nowhere in the application.
const DOC_TITLE_ANALYTICS_ID: &str = "Analytics ID";
const ROW_DELETE: &str = "Delete all local data";
/// What the confirmation says under its question. The verb is accurate about what it removes and
/// says nothing about what it cannot reach, and this is the last moment that difference can be
/// stated: after the press the app signs out and the screen is gone. The four recipients are the
/// ones the privacy policy names, in the same order, because a person who reads both should not
/// have to reconcile two lists.
const DELETE_SCOPE: &str = "This signs out and removes PlxNative data stored on this television. It does not delete data already sent to Plex, your Plex Media Servers, Sentry or PostHog.";
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
    /// Item 14: the crash/error channel's own preview — see `DOC_TITLE_CRASH`.
    PreviewCrash,
    /// Item 14: the usage/analytics channel's own preview — see `DOC_TITLE_USAGE`.
    PreviewUsage,
    Policy,
    /// The PostHog identifier and how to have those events deleted — see [`DOC_TITLE_ANALYTICS_ID`].
    AnalyticsId,
    Delete,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    FirstRun,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Crash,
    Product,
}
/// Which document the pushed reader is showing. Item 14 split the old single `Payload` kind into
/// one per telemetry channel so the two can never bleed into each other's document.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PreviewKind {
    Crash,
    Usage,
    Policy,
    AnalyticsId,
}

fn row_ids() -> Vec<RowId> {
    if mode() == Mode::FirstRun {
        // The two ANSWERS are not rows — they are the route's action band. What is left here is
        // only what you may READ before answering. Only ONE preview row shows, and it is the
        // channel this stage is actually asking about — `on_ok`'s `RowId::PreviewCrash`/
        // `PreviewUsage` arms both open a document, so which variant appears here is what decides
        // which channel's payload the person sees while deciding.
        let preview = if stage() == Stage::Crash {
            RowId::PreviewCrash
        } else {
            RowId::PreviewUsage
        };
        return vec![preview, RowId::Policy];
    }
    vec![
        RowId::Errors,
        RowId::Usage,
        RowId::PreviewCrash,
        RowId::PreviewUsage,
        RowId::Policy,
        RowId::AnalyticsId,
        RowId::Delete,
    ]
}

// ---- state -----------------------------------------------------------------------------------

static mut POP: Popover = Popover::new();
static mut TABLE: TableView = TableView::new(); // Settings mode only — see `list()`
/// **First run's two stages, each its own table.** Unlike Settings' `TABLE`, these hold FIXED
/// content (no toggle values, nothing draft-dependent), built once by [`build_first_run_tables`]
/// and never rebuilt again — which is exactly what lets both exist at once. `STAGE_PUSH` needs the
/// OUTGOING stage's real content still on screen while the incoming one arrives, and a single
/// shared table cannot provide that: the moment `choose`/`on_back` flips [`STAGE`], its rows would
/// already read as the new question while the departing side was still fading past it.
static mut TABLE_CRASH: TableView = TableView::new();
static mut TABLE_PRODUCT: TableView = TableView::new();
/// The answer being composed. Settings mirrors the stored decision and commits it through Done;
/// first run starts empty and commits both choices only after the second question. It is never
/// written straight to `consent::CURRENT`, so a half-made choice cannot let events through.
static mut DRAFT: (bool, bool) = (false, false);
static mut BASE: (bool, bool) = (false, false);
static mut MODE: Mode = Mode::FirstRun;
static mut STAGE: Stage = Stage::Crash;
static mut PREVIEW_KIND: PreviewKind = PreviewKind::Crash;
static mut DELETE_REQUESTED: bool = false;
static mut DOCUMENT_OPEN: bool = false;
static mut DOCUMENT_MORPH: RoutePush = RoutePush::new();
/// **Crash → Product, as a real push** — Crash is always the fixed PARENT role and Product the
/// fixed CHILD, exactly as `legal.rs`'s index/document pair never swap roles: BACK just runs the
/// amount from 1 back to 0, it never relabels which stage is which. See `draw_stage`.
static mut STAGE_PUSH: RoutePush = RoutePush::new();
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
/// First run's two answers' shared press surface (index 0 = Share, 1 = Don't share — the same
/// order `draw_action_row`'s loop draws them in). Before this, the two buttons drew through
/// `Button::focused()` alone: a colour change on arrival, no pop, no press dip on a click.
static mut ANSWER_POP: crate::ui::route_screen::ActionRow<2> =
    crate::ui::route_screen::ActionRow::new();
/// Settings' single Done control's own press surface, same reason.
static mut DONE_POP: crate::ui::route_screen::ActionRow<1> = crate::ui::route_screen::ActionRow::new();

#[allow(static_mut_refs)]
fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}
#[allow(static_mut_refs)]
fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}
#[allow(static_mut_refs)]
fn table_crash() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE_CRASH) }
}
#[allow(static_mut_refs)]
fn table_product() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE_PRODUCT) }
}

/// The list actually on screen this frame — the shared Settings table, or whichever first-run
/// stage [`STAGE_PUSH`] currently shows. Every UP/DOWN, OK-on-a-row and pointer hit-test goes
/// through this rather than a raw static, so input can never act on a table that split-second push
/// motion has already carried off screen.
fn list() -> &'static mut TableView {
    match mode() {
        Mode::Settings => table(),
        Mode::FirstRun => match stage() {
            Stage::Crash => table_crash(),
            Stage::Product => table_product(),
        },
    }
}

/// Build first run's two fixed-content lists once. Both are pure functions of which channel each
/// stage previews — never of `draft`/`errors`/`usage`, which is what lets them be built once at
/// [`open`] and never rebuilt for the life of the ceremony (compare Settings' `TABLE`, whose rows
/// carry live toggle values and must rebuild on every change).
fn build_first_run_tables() {
    fn seed(t: &mut TableView) {
        t.header_ink = theme::TEXT_READING;
        // Same two labels on both stages — `ROW_EXAMPLE` names no channel, since the row's own
        // wording is generic ("See an example report") and it is `row_ids()`/`on_ok`'s job to
        // send the click to the channel THIS stage is actually asking about (item 14).
        t.set_sections(
            vec![Section::new("")
                .row(Row::new(ROW_EXAMPLE).chevron(true))
                .row(Row::new(ROW_POLICY).chevron(true))],
            0,
            false,
        );
        // First run always opens focused on the ANSWERS, never on this reading list — see
        // `enter_first_run_stage`, which restates this every time a stage becomes current so a
        // rebuild is never the only place the invariant holds.
        t.list_focused = false;
    }
    seed(table_crash());
    seed(table_product());
}

/// Re-seat focus on the CURRENT first-run stage's own two answers. Called on `open` and again
/// whenever `choose`/`on_back` change which stage is showing, so a stage change is always an
/// arrival at its own question rather than inheriting whatever the previous one left the cursor on
/// — the same rule [`rebuild_with_motion`]'s old first-run branch stated, kept here now that the
/// two stages are no longer one shared table to write it onto.
fn enter_first_run_stage() {
    unsafe {
        ACTION_FOCUSED = true;
        ANSWER_SHARE = true;
    }
    table_crash().list_focused = false;
    table_product().list_focused = false;
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

/// Whether the route's focus is currently on a CONTROL FACE — first run's Share/Don't-share, or
/// Settings' Done — rather than a `TableView` row. The same shape `onboard::focus_is_ctl` and
/// `profiles::focus_is_ctl` already answer for their own screens: a caller (`app.rs`'s
/// `key_onboarding`-style dispatch) arms [`crate::ui::press::begin_ctl`] on OK-down exactly when
/// this is `true`, so the control dips and rings back instead of activating flat on the key-down.
///
/// Wired from `app.rs`'s consent arm (the popover chain above the route arms): OK arms the press
/// when this is `true` and `commit_consent` runs on the spring-back; a document row still commits
/// on its key-down.
pub(crate) fn focus_is_ctl() -> bool {
    menu_open()
        && !preview_open()
        && !delete_alert().is_open()
        && action_visible()
        && unsafe { *addr_of!(ACTION_FOCUSED) }
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
        (*addr_of_mut!(STAGE_PUSH)).jump(false);
        ACTION_FOCUSED = false;
        (*addr_of_mut!(GROUND)).reset();
        GROUND_DRAWN = false;
    }
    reader().reset();
    delete_alert().close();
    build_first_run_tables();
    enter_first_run_stage();
    pop().open();
    crate::ui::idle::invalidate();
}

/// Select the second first-run purpose for a deterministic visual/performance boot.
///
/// This changes presentation state only: it neither answers nor records either choice. Keeping
/// the harness seam here means the app cannot construct a half-valid consent draft of its own.
pub(crate) fn show_product_for_dev() {
    if menu_open() && mode() == Mode::FirstRun {
        unsafe {
            addr_of_mut!(STAGE).write(Stage::Product);
            // A boot trigger lands directly on this stage rather than pressing through Crash, so
            // it jumps the push instead of animating it — see `route_screen`'s module doc on when
            // `jump` is the right call.
            (*addr_of_mut!(STAGE_PUSH)).jump(true);
        }
        enter_first_run_stage();
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

/// Rebuild the SETTINGS rows against the current draft. Called on every toggle, because a
/// `TableView` holds its rows by value — the checkmark is state in the row, not a live read.
///
/// **Settings only, now.** First run's two stages carry no draft-dependent value (no toggle, no
/// sub-line that changes), so they no longer rebuild at all — `build_first_run_tables` seeds both
/// once at `open` and `enter_first_run_stage` only ever moves focus between them. See `TABLE_CRASH`
/// / `TABLE_PRODUCT`'s doc for why a stage change needed two tables instead of one shared rebuild.
fn rebuild(sel: i32) {
    rebuild_with_motion(sel, true);
}

fn rebuild_initial(sel: i32) {
    rebuild_with_motion(sel, false);
}

fn rebuild_with_motion(sel: i32, preserve_motion: bool) {
    debug_assert_eq!(mode(), Mode::Settings, "first run no longer rebuilds a shared table");
    let (errors, usage) = draft();
    table().header_ink = theme::TEXT_READING;
    let reporting = Section::new("Reporting")
        .row(Row::new(ROW_ERRORS).detail(ROW_ERRORS_SUB).toggle(errors))
        .row(Row::new(ROW_USAGE).detail(ROW_USAGE_SUB).toggle(usage));
    // Item 14: two rows, one per telemetry channel, each opening ITS OWN document — a
    // combined preview left it unclear which report actually carried which field.
    let info = Section::new("Information")
        .row(
            Row::new(DOC_TITLE_CRASH)
                .detail("Field-by-field preview of the crash/error report.")
                .chevron(true),
        )
        .row(
            Row::new(DOC_TITLE_USAGE)
                .detail("Field-by-field preview of product analytics events.")
                .chevron(true),
        )
        .row(
            Row::new(ROW_POLICY)
                .detail("The complete PlxNative privacy policy for this build.")
                .chevron(true),
        )
        .row(
            Row::new(DOC_TITLE_ANALYTICS_ID)
                .detail("The identifier on your analytics, and how to have it deleted.")
                .chevron(true),
        );
    let local = Section::new("On this TV").row(
        Row::new(ROW_DELETE)
            .detail("Sign out and remove PlxNative data from this TV.")
            .chevron(true),
    );
    table().set_sections(vec![reporting, info, local], sel, preserve_motion);
    // Settings opens on the LIST — `TABLE` parks `list_focused = false` while a document reader
    // is open over it, so this restores it every rebuild rather than assuming it survived.
    table().list_focused = true;
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
            enter_first_run_stage();
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
        enter_first_run_stage();
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
    let sel = list().sel.clamp(0, rows.len() as i32 - 1);
    match rows[sel as usize] {
        RowId::Errors => {
            let (e, u) = draft();
            unsafe { addr_of_mut!(DRAFT).write((!e, u)) };
            rebuild(sel);
            unsafe { ACTION_FOCUSED = true };
            list().list_focused = false;
            crate::ui::idle::invalidate();
        }
        RowId::Usage => {
            let (e, u) = draft();
            unsafe { addr_of_mut!(DRAFT).write((e, !u)) };
            rebuild(sel);
            unsafe { ACTION_FOCUSED = true };
            list().list_focused = false;
            crate::ui::idle::invalidate();
        }
        RowId::PreviewCrash => {
            unsafe {
                PREVIEW_KIND = PreviewKind::Crash;
                DOCUMENT_OPEN = true;
            }
            reader().reset();
            crate::ui::idle::invalidate();
        }
        RowId::PreviewUsage => {
            unsafe {
                PREVIEW_KIND = PreviewKind::Usage;
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
        RowId::AnalyticsId => {
            unsafe {
                PREVIEW_KIND = PreviewKind::AnalyticsId;
                DOCUMENT_OPEN = true;
            }
            reader().reset();
            crate::ui::idle::invalidate();
        }
        RowId::Delete => {
            delete_alert().open_with_body(DELETE_SCOPE);
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
                list().list_focused = true;
                crate::ui::idle::invalidate();
            }
            return true;
        }
        if action_visible() && delta > 0 && list().sel == list().n_rows() - 1 {
            unsafe { ACTION_FOCUSED = true };
            list().list_focused = false;
            crate::ui::idle::invalidate();
            return true;
        }
        list().move_sel(delta);
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
        (*addr_of_mut!(STAGE_PUSH)).update(stage() == Stage::Product, dt);
    }
    reader().update(dt);
    // `sel` changes immediately so the focused row's ink can change in the same frame; the white
    // plate is a pair of springs and only advances here. Omitting this made Privacy the lone
    // Settings child whose text moved while its plate stayed where the screen opened.
    let layout = RouteLayout::screen();
    let action_focused = unsafe { *addr_of!(ACTION_FOCUSED) };
    if mode() == Mode::Settings {
        table().update(dt, layout.sectioned_table().h);
        unsafe { (*addr_of_mut!(DONE_POP)).step(action_focused.then_some(0), dt) };
    } else {
        // Both stages, not just the current one: `STAGE_PUSH` keeps the outgoing stage's own
        // table on screen (fading/sliding) for the whole transition, so both must stay warm.
        table_crash().update(dt, layout.content.h);
        table_product().update(dt, layout.content.h);
        let on_share = unsafe { *addr_of!(ANSWER_SHARE) };
        let focused_index = action_focused.then_some(if on_share { 0 } else { 1 });
        unsafe { (*addr_of_mut!(ANSWER_POP)).step(focused_index, dt) };
    }
}

// ---- the payload previews ----------------------------------------------------------------------
//
// Item 14: this used to be ONE function (`preview`) concatenating both channels into a single
// document, so it was unclear which report actually carried which field. It is now two functions,
// one per channel, each opening its own reader — `preview_crash` for Sentry, `preview_usage` for
// PostHog. `preview()` still exists, `#[cfg(test)]` only, as the UNION the invariant tests grade
// (every event/field this build can emit must appear in ONE of the two channel documents); no
// screen ever shows that concatenation to a person.

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
         sent.\n\n",
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
         reporting is on, with a random installation identifier. Random and build-specific values \
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

/// The union of both channels — test-only. Nothing on screen shows this concatenation to a person;
/// Settings and first run always open exactly one channel's own document (`preview_crash` or
/// `preview_usage`). It exists purely so `the_preview_shows_every_event_this_build_can_emit` can
/// keep asserting over "everything this build can send" without caring which of the two channels a
/// given event belongs to.
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
            "YOUR ANALYTICS ID\n\n{id}\n\nWHAT IT IS\n\nA random identifier created on this television when you turned product analytics on. It is attached to analytics events so they can be counted as coming from one installation. It is not derived from your Plex account, your television or anything about you, and it is never sent with crash reports.\n\nHOW TO HAVE THESE EVENTS DELETED\n\nWrite to {CONTACT_EMAIL} and quote the identifier above. It is the only handle these events carry, so a request without it cannot be matched to anything.\n\nHOW IT ENDS\n\nTurning product analytics off deletes this identifier, and turning analytics on again creates a different one. Delete all local data removes it as well. Events already sent keep the old identifier, which is why it is worth copying down before you turn analytics off if you intend to ask for their deletion."
        ),
        None => format!(
            "NO ANALYTICS ID\n\nProduct analytics is off, so this installation has no analytics identifier and is sending no analytics events.\n\nAn identifier is created only when you turn product analytics on, and deleting it is what turning it off does. If you had analytics on before and want events from that period deleted, write to {CONTACT_EMAIL} — but note that the identifier they carry was destroyed when analytics was turned off, so it can no longer be looked up from this television.\n\nCrash reports carry no installation or analytics identifier. (Each report has its own event id, and some carry a fingerprint grouping like reports together, but neither is tied to this installation or to you.)"
        ),
    }
}

fn privacy_policy() -> &'static str {
    crate::ui::legal::Page::Privacy.body()
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

/// Where BACK goes from `which` stage, named on the crumb above the title — `None` where BACK goes
/// nowhere. Takes an explicit `Stage` rather than reading the global one because [`draw_stage`]
/// needs BOTH stages' crumbs during a transition, not only whichever is current.
fn crumb_for(mode: Mode, which: Stage) -> Option<&'static str> {
    match (mode, which) {
        (Mode::Settings, _) => Some(CRUMB_SETTINGS),
        // The ROOT of the ceremony: sign-in is behind it and cannot be undone, so there is
        // nothing honest to name. See the module doc.
        (Mode::FirstRun, Stage::Crash) => None,
        (Mode::FirstRun, Stage::Product) => Some(CRASH_TITLE),
    }
}

/// Where BACK goes from the CURRENT route, for a caller that only ever means "right now" — the
/// pushed document panel, and the tests that pin the census.
fn crumb() -> Option<&'static str> {
    crumb_for(mode(), stage())
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
        .scale(unsafe { (*addr_of!(DONE_POP)).scale(0) })
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
    for (i, (label, rect, is_share)) in [(share, share_r, true), (decline, decline_r, false)]
        .into_iter()
        .enumerate()
    {
        Button::new(label.as_ptr(), theme::size::BODY, rect)
            .focused(focused && on_share == is_share)
            .scale(unsafe { (*addr_of!(ANSWER_POP)).scale(i) })
            .palette(palette)
            .draw(&Env::inert(), p);
    }
}

/// Whether `which` stage still has anything worth drawing at this `amount` of [`STAGE_PUSH`] —
/// pure, so the mid-transition claim "both stages are on screen at once" is a host-testable fact
/// rather than something only a captured frame sequence can show. Crash is the parent (visible
/// while the push has not fully completed) and Product the child (visible once it has begun) —
/// the same fixed-role split `draw_stage` draws with.
fn stage_visible(which: Stage, amount: f32) -> bool {
    match which {
        Stage::Crash => amount < 0.999,
        Stage::Product => amount > 0.001,
    }
}

/// Draw one first-run stage's narrative + table + (for the current stage only) action row, pushed
/// through [`STAGE_PUSH`] under its FIXED role — Crash always via
/// [`RoutePush::parent`](crate::ui::route_screen::RoutePush::parent), Product always via
/// [`RoutePush::child`](crate::ui::route_screen::RoutePush::child) — exactly the way `legal.rs`
/// never swaps its index/document roles when BACK runs the same spring backward.
///
/// Both stages have their own real, unrebuildable table ([`TABLE_CRASH`]/[`TABLE_PRODUCT`]), which
/// is what lets the OUTGOING stage's content still be on screen, correctly, while the incoming one
/// arrives — a single shared table would already have been overwritten the instant `choose`/
/// `on_back` flipped [`STAGE`], long before the animation finished showing it leave.
fn draw_stage(route_layer: crate::ui::Painter, layout: RouteLayout, which: Stage) {
    let amount = unsafe { (*addr_of!(STAGE_PUSH)).amount() };
    if !stage_visible(which, amount) {
        return;
    }
    let p = match which {
        Stage::Crash => unsafe { (*addr_of!(STAGE_PUSH)).parent(route_layer) },
        Stage::Product => unsafe { (*addr_of!(STAGE_PUSH)).child(route_layer) },
    };
    let (stage_title, stage_body, stage_table) = match which {
        Stage::Crash => (CRASH_TITLE, CRASH_BODY, table_crash()),
        Stage::Product => (PRODUCT_TITLE, PRODUCT_BODY, table_product()),
    };
    layout.draw_narrative(p, crumb_for(Mode::FirstRun, which), stage_title, stage_body, theme::size::BODY);
    stage_table.draw(p, layout.content);
    if which == stage() && !preview_open() {
        draw_action_row(p, layout);
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
    // The layer BELOW the pushed document — either mode's own route content rides here, so the
    // document push nests cleanly outside whichever stage push first run is also running.
    let route_layer = unsafe { (*addr_of!(DOCUMENT_MORPH)).parent(entrance) };
    let layout = RouteLayout::screen();
    if mode() == Mode::Settings {
        layout.draw_narrative(
            route_layer,
            crumb(),
            SETTINGS_TITLE,
            "Control optional reporting, review exactly what may be shared, and manage data stored by PlxNative on this television.",
            theme::size::LABEL,
        );
        table().draw(route_layer, layout.sectioned_table());
        if !preview_open() {
            draw_action_row(route_layer, layout);
        }
    } else {
        // Crash is the fixed PARENT role and Product the fixed CHILD — `STAGE_PUSH` never
        // relabels which is which, so both are always drawn (each conditionally, at its own
        // visibility threshold) rather than picking one by "which is current".
        draw_stage(route_layer, layout, Stage::Crash);
        draw_stage(route_layer, layout, Stage::Product);
    }

    if t > 0.01 {
        let kind = unsafe { *addr_of!(PREVIEW_KIND) };
        let dp = unsafe { (*addr_of!(DOCUMENT_MORPH)).child(entrance) };
        // Item 14: three documents now, not two — the crash channel's own preview, the usage
        // channel's own preview, and the policy — each with its own title and subtitle so the
        // reader can never mistake which document (or which channel) it is looking at.
        let (doc_title, subtitle): (&str, &str) = match kind {
            PreviewKind::AnalyticsId => (
                DOC_TITLE_ANALYTICS_ID,
                "The random identifier attached to product analytics from this installation, and how to have those events deleted.",
            ),
            PreviewKind::Policy => (
                ROW_POLICY,
                "How PlxNative handles local data, Plex services and optional reporting.",
            ),
            // The document's TITLE is the channel alone: the row's long form ("… — what is
            // actually sent") elides to "…what is actuall…" at the route title's rung inside the
            // narrative column (sim, 2026-09-02), so the "what is actually sent" half moves into
            // the subtitle, where it has the room to be a sentence.
            PreviewKind::Crash => (
                DOC_TITLE_CRASH,
                "What is actually sent: the exact fields a crash or error report can carry — only when error reporting is on.",
            ),
            PreviewKind::Usage => (
                DOC_TITLE_USAGE,
                "What is actually sent: the exact fields a product analytics event can carry — only when usage reporting is on.",
            ),
        };
        // A pushed document's crumb names the question it was opened from, so the way back out of
        // a read is stated even on the route that has no BACK hint anywhere.
        layout.draw_narrative(
            dp,
            Some(if mode() == Mode::Settings {
                SETTINGS_TITLE
            } else {
                title()
            }),
            doc_title,
            subtitle,
            theme::size::LABEL,
        );
        let text = match kind {
            PreviewKind::AnalyticsId => analytics_id_document(),
            PreviewKind::Policy => privacy_policy().to_string(),
            PreviewKind::Crash => preview_crash(),
            PreviewKind::Usage => preview_usage(),
        };
        reader().draw(dp, layout.content, None, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The two Privacy-policy doors must open ONE document.** This screen's Information section
    /// and `ui::legal`'s index both offer "Privacy policy", and this row's own detail line calls
    /// what it opens "The complete PlxNative privacy policy for this build" — a claim only the
    /// canonical text can satisfy. They were two separate literals, and they had already drifted:
    /// this module's copy was missing the whole `ON THIS TELEVISION` section, so the shorter of the
    /// two was the one describing itself as complete. Nothing compiles a `&'static str` against
    /// another, so only this assertion can hold them together.
    #[test]
    fn both_privacy_policy_doors_open_the_same_document() {
        assert_eq!(
            privacy_policy(),
            crate::ui::legal::Page::Privacy.body(),
            "the policy shown from Privacy & data must BE the policy shown from Legal notices"
        );
    }

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
        // Settings' TABLE, explicitly: `rebuild_with_motion` only ever writes it in this mode now
        // that first run's two stages have their own fixed, unrebuilt tables (`TABLE_CRASH`/
        // `TABLE_PRODUCT`) — see their doc.
        unsafe { addr_of_mut!(MODE).write(Mode::Settings) };
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
        unsafe { addr_of_mut!(MODE).write(Mode::Settings) };
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

    /// **Item 14's whole point: the crash document carries nothing from the usage channel.** A
    /// person reading "what crash reports send" must never see a PostHog identifier or event
    /// field — that would make the split cosmetic rather than a real separation of what goes
    /// where.
    #[test]
    fn the_crash_preview_carries_nothing_from_the_usage_channel() {
        let text = preview_crash();
        assert!(
            !text.contains("distinct_id"),
            "no PostHog envelope field belongs in the crash-only document"
        );
        assert!(
            !text.contains("<project key>"),
            "no PostHog project key belongs in the crash-only document"
        );
        for s in crate::diag::schema::EVENT_SPECS {
            // The `"event": "<name>"` PAIR (`posthog.rs`'s own shape), not a bare substring or
            // even a bare quoted name: the handled-playback-error (Sentry) schema independently
            // uses several of the SAME strings as ordinary field keys — `"playback.started":
            // "yes"` is a real, correct line in the crash preview, and `playback.requested` is a
            // prefix of the crash-only field `playback.requested_quality`. Only the full
            // event-envelope pair is unique to a usage document.
            let quoted = format!("\"event\": \"{}\"", s.name);
            assert!(
                !text.contains(&quoted),
                "usage event `{}` leaked into the crash preview",
                s.name
            );
        }
    }

    /// **The mirror image: the usage document carries nothing from the crash channel.** No Sentry
    /// envelope shape, no exception/thread/register data — a person reading "what analytics send"
    /// must not be shown fields that only a crash report can carry.
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
            assert!(
                !text.contains(sentry_only),
                "crash-only field `{sentry_only}` leaked into the usage preview"
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
        // `DRAFT`, `TABLE` and the first-run tables are crate globals, so this takes the shared
        // lock rather than a local one — `[[test-suite-global-pollution]]`'s rule, and the reason
        // the whole `auth` block once aborted under load.
        let _g = crate::testlock::serial();
        unsafe {
            addr_of_mut!(MODE).write(Mode::FirstRun);
            addr_of_mut!(STAGE).write(Stage::Crash);
            addr_of_mut!(BASE).write((false, false));
            addr_of_mut!(DRAFT).write((false, false));
        }
        build_first_run_tables();
        // COUNT and ORDER. A count alone passes on two rows swapped, or on one replaced by
        // another — which is exactly the index bug the doc comment above describes, so counting was
        // never enough to catch it.
        assert_eq!(list().n_rows(), row_ids().len() as i32, "first run, Crash stage");
        assert_eq!(row_ids(), vec![RowId::PreviewCrash, RowId::Policy], "Crash stage");

        // The Product stage was never exercised here at all, so the one asymmetry in `row_ids`
        // that a stage CAN get wrong — which channel's preview it offers — was ungraded.
        unsafe { addr_of_mut!(STAGE).write(Stage::Product) };
        build_first_run_tables();
        assert_eq!(list().n_rows(), row_ids().len() as i32, "first run, Product stage");
        assert_eq!(row_ids(), vec![RowId::PreviewUsage, RowId::Policy], "Product stage");

        unsafe { addr_of_mut!(MODE).write(Mode::Settings) };
        rebuild(0);
        assert_eq!(list().n_rows(), row_ids().len() as i32, "settings");
        assert_eq!(
            row_ids(),
            vec![
                RowId::Errors,
                RowId::Usage,
                RowId::PreviewCrash,
                RowId::PreviewUsage,
                RowId::Policy,
                RowId::AnalyticsId,
                RowId::Delete,
            ],
            "settings"
        );
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
        // The invariant is that the answer's arrival does not RESIZE the list, so the count is
        // taken here rather than written down: a literal census had to be edited by every change
        // that legitimately adds a row, and an edited expectation grades nothing.
        //
        // It must be the TABLE's count, not `row_ids()`'s. `row_ids` is a function of mode and
        // stage alone and cannot move when a draft changes, so comparing it to itself across the
        // rebuild asserted nothing at all — a Done row appended to `TableView` would have passed.
        let rows_before = list().n_rows();
        unsafe { addr_of_mut!(DRAFT).write((true, false)) };
        rebuild(0);
        assert!(action_visible());
        assert_eq!(list().n_rows(), rows_before, "Done never changes table geometry");
        assert_eq!(
            list().n_rows(),
            row_ids().len() as i32,
            "and the id map still describes the rebuilt table"
        );
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
            vec![RowId::PreviewCrash, RowId::Policy],
            "only the two readable documents remain in the list, and the preview is the \
             crash channel's own — Stage::Crash is where a fresh question always starts"
        );
        assert_eq!(list().n_rows(), 2);
        assert!(
            action_visible(),
            "first run always carries its answers in the band"
        );
        assert!(
            unsafe { *addr_of!(ACTION_FOCUSED) },
            "focus opens on the answers, not on the reading list"
        );
        assert!(!list().list_focused, "so no row is plated meanwhile");
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
        assert!(list().list_focused);
        list().sel = list().n_rows() - 1;
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
        assert!(!list().list_focused, "first run parks it here");
        close();
        open_settings(&Consent::default());
        assert!(
            list().list_focused,
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

    /// **Crash → Product is a real push, not a cut.** Before `STAGE_PUSH` existed, answering Crash
    /// swapped the whole route's text on the very next frame with no transition at all — the same
    /// document-push spring `consent.rs` already used for its own preview reader, just never
    /// reused for the stage change beside it. This is the host-testable half of "both pages must
    /// move": mid-transition, BOTH stages are visible (one fading/travelling out, one arriving),
    /// and at either endpoint only the settled one is.
    #[test]
    fn crash_to_product_is_a_push_where_both_stages_are_visible_mid_transition() {
        let _g = crate::testlock::serial();
        open(&Consent::default());
        assert!(
            stage_visible(Stage::Crash, 0.0) && !stage_visible(Stage::Product, 0.0),
            "settled on Crash: only Crash draws"
        );
        choose(true); // -> Stage::Product, STAGE_PUSH now animating toward 1.0
        for _ in 0..3 {
            update(1.0 / 60.0);
        }
        let mid = unsafe { (*addr_of!(STAGE_PUSH)).amount() };
        assert!(mid > 0.0 && mid < 1.0, "still mid-flight after 3 frames: {mid}");
        assert!(
            stage_visible(Stage::Crash, mid) && stage_visible(Stage::Product, mid),
            "mid-transition: BOTH the departing Crash and the arriving Product must be on screen"
        );
        for _ in 0..600 {
            update(1.0 / 60.0);
        }
        let settled = unsafe { (*addr_of!(STAGE_PUSH)).amount() };
        assert!(
            stage_visible(Stage::Product, settled) && !stage_visible(Stage::Crash, settled),
            "settled on Product: only Product draws"
        );
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
