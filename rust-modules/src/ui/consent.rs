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
//! # It is asked ONCE PER SIGN-IN, before the profile picker
//!
//! **Consent here is the signed-in ACCOUNT's decision, not a per-viewer preference and not the
//! television's**: `telemetry_candidates()` is one file with no profile key, so whoever answers
//! binds every profile on the account — and `auth::forget_account` unlinks that file and destroys
//! both identifiers when the account signs out, so the next account to sign in through the QR
//! flow is asked afresh (until 2026-09-04 the decision outlived the sign-in, and a second account
//! was reported under the first one's answer and identifiers). Until 2026-09-02 it was asked on
//! first arrival at Home — i.e. *after* who's-watching — which put a data-protection question to
//! whichever household member happened to be picked, up to and including a managed child profile,
//! and made an account-wide answer look like a personal setting. It is now asked as soon as there
//! is an authorized account and before the picker, so the person who signed the television in is
//! the person who answers.
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
use crate::ui::route_screen::{
    PressFrom, RouteFocus, RouteGround, RouteLayout, RoutePush, RouteShape, RouteStep,
};
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
const CRASH_BODY: &str = "If PlxNative crashes, it can send technical details that help find and fix the problem. Reports may include the signal, code addresses, thread information and device compatibility details, plus a random crash report identifier, created when you turn this on and deleted when you turn it off or sign out, so that repeated crashes under one crash report identifier are counted once rather than once each. They never include titles, Plex accounts, searches, server names or addresses, tokens, subtitle text, or the product analytics identifier.";
const PRODUCT_BODY: &str = "PlxNative can share which screens and features are used and broad sign-in and playback outcomes. Reports carry a random Analytics ID, created when you turn this on and deleted when you turn it off or sign out, and can include the app version, webOS version, television model and SoC, and whether a selected server is local, remote or relayed. They never include titles, Plex accounts, searches, server names or addresses, tokens, subtitle text, or exact viewing history.";

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
/// The row that shows the PostHog analytics identifier, and the title of the document it opens.
/// It exists so that "write to us and ask for your analytics to be deleted" is an instruction a
/// person can actually follow: the identifier is the only handle those events have, and before this
/// row it was minted, persisted and sent while being visible nowhere in the application.
const DOC_TITLE_ANALYTICS_ID: &str = "Analytics ID";
/// The crash channel's twin: the Sentry `user.id`, shown for the same reason and with the same
/// deletion instruction. Two rows rather than one document listing both, because they are two
/// consents with two lifetimes, and a person who turned only one of them on should find exactly
/// one identifier here.
const DOC_TITLE_ERRORS_ID: &str = "Crash report ID";
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
    /// The Sentry identifier and how to have those reports deleted — see [`DOC_TITLE_ERRORS_ID`].
    ErrorsId,
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
    ErrorsId,
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
        RowId::ErrorsId,
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
/// **Where focus is, in the family's shared terms** — `ui::route_screen`'s [`RouteFocus`], not a
/// pair of private booleans. It replaced `ACTION_FOCUSED` (is the band focused) and `ANSWER_SHARE`
/// (which answer the ring is on), which between them could express states the screen has no way to
/// draw: an `ACTION_FOCUSED` band that the same edit had just removed was the reported
/// "focus disappears completely". The band cursor is still a CURSOR rather than a pre-selected
/// value — the two first-run answers are equals, nothing times out onto either, and `draft()` is
/// untouched until OK is actually pressed.
static mut FOCUS: RouteFocus = RouteFocus::content();
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
    unsafe { addr_of_mut!(FOCUS).write(RouteFocus::band()) };
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

/// Where focus is, in the family's shared terms — see `ui::route_screen`'s rule list.
fn focus() -> RouteFocus {
    unsafe { *addr_of!(FOCUS) }
}

/// Which band control an in-flight press was armed on, and where the press came from — `None` for
/// no press.
///
/// **Read through [`armed`], never raw.** `ui::press` is a crate-global machine that many things
/// can cancel (a nav key, a fresh click, the lost-key-up ceiling), and none of them knows this
/// static exists — so a local arm that outlived its press would silently judge the NEXT one. The
/// accessor expires it against `press::is_live()`, which makes staleness impossible by
/// construction rather than by finding every cancel site. **`is_live`, not `is_active`**: a cancel
/// only clears the commit and leaves the press on screen for its bounce, so an arm expired against
/// `is_active` outlives the press that owned it by ~200 ms — long enough for the next immediate
/// activation to be judged against it (Codex review, 2026-09-04).
static mut ARMED: Option<(PressFrom, usize)> = None;
/// …and the delete alert's own, which is a separate door for the SDL2_ttf reason
/// [`alert_press_at`] records.
static mut ARMED_ALERT: Option<AlertChoice> = None;

/// **Which band control is under the pointer** — pure hit-testing, no focus moved. `None` for dead
/// space, which is the whole point: `ui::press`'s model is that focus cannot move mid-press, and
/// the guard has to see the pointer leaving EVERYTHING, not only its arrival somewhere else.
fn band_hit(mx: f32, my: f32) -> Option<usize> {
    if !action_visible() {
        return None;
    }
    if mode() == Mode::FirstRun {
        unsafe { (*addr_of!(ANSWER_POP)).hit(mx, my) }
    } else {
        unsafe { (*addr_of!(DONE_POP)).hit(mx, my) }
    }
}

/// The live arm, or `None` — expired against the crate-global press machine, so an arm can never
/// outlive the press it belongs to.
fn armed() -> Option<(PressFrom, usize)> {
    crate::ui::press::is_live()
        .then(|| unsafe { *addr_of!(ARMED) })
        .flatten()
}

/// Record that a press on the focused band control has been armed from the OK KEY. `app.rs` calls
/// it beside `press::begin_ctl`; without it a key press has no local identity at all and the very
/// first pointer motion cancels it.
pub(crate) fn arm_key() {
    let i = focus().band_index();
    unsafe { addr_of_mut!(ARMED).write(i.map(|i| (PressFrom::Key, i))) };
}

/// Whether an armed press is STILL on the thing it was armed on — `app.rs` cancels when it is not.
/// Parks focus on the way past, so this is also the ordinary hover path.
///
/// **The two origins are judged differently, and that is the point** ([`PressFrom`]). A press that
/// began under the pointer is bound to the CONTROL the pointer is over, `None` included — the
/// first version of this guard compared the focused stop before and after, and a miss leaves focus
/// exactly where it was, so pointer-down on `Share reports` and then sliding off every control
/// still recorded the answer. A press that began on the OK key is bound to the FOCUS STOP, so
/// hover onto the other answer cancels it while hover across dead space, which moves no focus,
/// does not.
pub(crate) fn pointer_hold(mx: f32, my: f32) -> bool {
    if delete_alert().is_open() {
        return false; // the alert has its own door — see `alert_hold`
    }
    let hit = layers_settled().then(|| band_hit(mx, my)).flatten();
    pointer_focus(mx, my);
    match armed() {
        Some((PressFrom::Pointer, i)) => hit == Some(i),
        Some((PressFrom::Key, i)) => focus().band_index() == Some(i),
        // Nothing of ours is armed, so nothing of ours is being retracted.
        None => true,
    }
}

/// This route's shape, as the shared rules see it. Read from live state on every key rather than
/// cached, because both of its interesting fields move under the screen: Settings' band appears
/// with the first edit and vanishes when it is undone, and a pushed document replaces the whole
/// content column with a scroller that has no rows at all.
fn shape() -> RouteShape {
    if preview_open() {
        return RouteShape::document();
    }
    let band = if !action_visible() {
        0
    } else if mode() == Mode::FirstRun {
        2 // Share / Don't share — two equals
    } else {
        1 // Done
    };
    let list = list();
    let rows = list.n_rows() > 0;
    RouteShape {
        band,
        rows,
        at_last_row: rows && list.at_last_row(),
        opens: rows && list.row_opens(list.sel),
        // Rule 9's guard, stated rather than inferred. Settings holds a DRAFT that BACK discards,
        // so LEFT may not overshoot Done into it. First run holds nothing — `on_back` records no
        // refusal, it walks Product → Crash and is swallowed at Crash — so LEFT off the leading
        // answer follows the crumb, which is the whole point of drawing one.
        uncommitted: mode() == Mode::Settings && draft() != base(),
    }
}

/// **Rule 10, applied here.** Re-settle focus against the shape the screen actually has now, then
/// make the table's plate agree with it — the two are ONE decision (`TableView::list_focused`
/// gates the ink as well as the pill), so a screen that writes one without the other draws two
/// accent capsules or none.
///
/// First run keeps BOTH stage tables in step rather than only the current one: `STAGE_PUSH` leaves
/// the outgoing stage's own table on screen for the whole transition, and a plate on a page that is
/// travelling out is exactly as wrong as one on a page that is not focused.
fn sync_focus() {
    let mut f = focus();
    f.settle(shape());
    unsafe { addr_of_mut!(FOCUS).write(f) };
    let on_list = f.on_content() && !preview_open();
    match mode() {
        Mode::Settings => table().list_focused = on_list,
        Mode::FirstRun => {
            table_crash().list_focused = on_list && stage() == Stage::Crash;
            table_product().list_focused = on_list && stage() == Stage::Product;
        }
    }
}

/// Perform what a shared rule decided. The focus half has already happened; this is the screen's
/// own half — which content column takes a vertical delta, what a row OPENS, and where BACK goes.
fn apply_step(step: RouteStep) -> bool {
    match step {
        RouteStep::Wall => {}
        RouteStep::Moved => {
            sync_focus();
            crate::ui::idle::invalidate();
        }
        RouteStep::Scroll(delta) => {
            if preview_open() {
                reader().move_by(delta);
            } else {
                list().move_sel(delta);
            }
            crate::ui::idle::invalidate();
        }
        RouteStep::Enter => {
            on_ok();
        }
        RouteStep::Back => {
            on_back();
        }
    }
    true
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
/// Wired from `app.rs`'s consent arm (the popover chain above the route arms) on BOTH input paths:
/// OK arms the press when this is `true` and `commit_consent` runs on the spring-back, and a
/// pointer-down goes through [`press_at`] to the same place. A document row still commits on its
/// key-down, and is [`click_row`]'s on the pointer.
pub(crate) fn focus_is_ctl() -> bool {
    menu_open()
        && !preview_open()
        && !delete_alert().is_open()
        && action_visible()
        && focus().band_index().is_some()
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

/// Put the sign-in's question on screen.
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
        addr_of_mut!(FOCUS).write(RouteFocus::content());
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
        // Settings opens on its LIST — rule 10's other half, and the reason this is written rather
        // than inherited: `FOCUS` is a crate global, and first run parks it on the answer band.
        addr_of_mut!(FOCUS).write(RouteFocus::content());
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
            Row::new(DOC_TITLE_ERRORS_ID)
                .detail("The identifier on your crash reports, and how to have them deleted.")
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
    // **Rule 10 on every rebuild.** This used to assert `list_focused = true` flatly, which was
    // right for the reason it said (a document reader parks it false) and wrong the moment the
    // band could hold focus: it drew a plate under a focused Done. `sync_focus` answers the same
    // question from the shape the rebuild has just produced — including the case that removed the
    // band, which is what left focus on nothing after a toggle was undone.
    sync_focus();
    debug_assert_eq!(row_ids().len() as i32, table().n_rows());
}

/// Commit the completed first-run decisions or the explicit Settings action, then close.
fn commit() {
    let (errors, usage) = draft();
    record_answer(errors, usage);
}

/// Record one explicit answer and close. BACK never reaches this function.
///
/// `consent::apply` owns the whole transition, including the refusal of a channel whose identifier
/// could not be minted; what is left here is saying so in the log, per channel, because the
/// person's answer and the recorded decision differ at that moment and nothing on screen says why.
fn record_answer(errors: bool, usage: bool) {
    let prev = consent::current().unwrap_or_default();
    let next = consent::apply(&prev, errors, usage, crate::telemetry::mint_id);
    for (asked, got, channel) in [
        (errors, next.errors, "crash reports"),
        (usage, next.usage, "usage analytics"),
    ] {
        if asked && !got {
            crate::log(&format!(
                "consent: no /dev/urandom — refusing {channel} rather than inventing an identifier"
            ));
        }
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
    close_delete_and_menu(false);
}

/// The shared teardown behind [`close`] and a CONFIRMED delete. The document reader and the outer
/// popover always end INSTANTLY here — their subject, the whole consent/Settings screen, really is
/// gone, exactly [`Popover::close`]'s case. The delete alert is the one piece that differs: a
/// defensive reset (`open`/`open_settings` calling `close()` before opening fresh) has never shown
/// the alert to anyone, so hiding it instantly changes nothing a person watched happen — but a
/// CONFIRMED delete has, and that answer must still play its own exit the way it played its entry.
/// See `decision_alert.rs`'s module doc for the rule this keeps: every interactive answer
/// dismisses, `close` is only for a subject that vanished with nothing to animate.
fn close_delete_and_menu(fade_delete_alert: bool) {
    unsafe {
        DOCUMENT_OPEN = false;
        ARMED = None;
        ARMED_ALERT = None;
        addr_of_mut!(FOCUS).write(RouteFocus::content());
        (*addr_of_mut!(ANSWER_POP)).clear();
        (*addr_of_mut!(DONE_POP)).clear();
    }
    reader().reset();
    if fade_delete_alert {
        delete_alert().dismiss();
        // The confirmed answer is the one exit that changes what stands BEHIND the alert while it
        // is still fading — `commit_consent` routes to Login the instant this call returns, but the
        // alert's own panel is `Glass::CACHED` (`DecisionAlert::new`'s default `Popover::new()`)
        // and, left alone, would keep serving the one snapshot it took of the SETTINGS page it
        // opened over for the whole of its exit: a ghost of the screen that is no longer there,
        // under text that is. Cancel and BACK never change what the alert stands on, so neither
        // takes this — `blur_invalidate` is a global "recapture next time", and calling it when
        // nothing changed would just spend a needless capture on the next frame.
        crate::gfx::blur_invalidate();
    } else {
        delete_alert().close();
    }
    if menu_open() {
        pop().close();
    }
    crate::ui::idle::invalidate();
}

/// BACK: a preview returns to its question, Settings discards, and first run reverses the wizard.
/// No first-run BACK writes [`DRAFT`] or persisted consent.
pub(crate) fn on_back() -> bool {
    if delete_alert().is_open() {
        delete_alert().dismiss();
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
        unsafe { ARMED_ALERT = None };
        if delete_alert().choice() == AlertChoice::Destructive {
            unsafe { addr_of_mut!(DELETE_REQUESTED).write(true) };
            close_delete_and_menu(true);
        } else {
            delete_alert().dismiss();
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
    if let Some(i) = focus().band_index().filter(|_| action_visible()) {
        unsafe { ARMED = None };
        match mode() {
            // index 0 is Share, 1 is Don't share — the order `draw_action_row` draws them in.
            Mode::FirstRun => choose(i == 0),
            Mode::Settings => commit(),
        }
        return true;
    }
    let rows = row_ids();
    let sel = list().sel.clamp(0, rows.len() as i32 - 1);
    match rows[sel as usize] {
        // **A flipped switch keeps focus.** These two arms used to park focus on Done, which was
        // reported twice over: it took the plate off the row being edited, and undoing the edit
        // then removed the Done the focus had been parked on and left the screen with no ring at
        // all. `rebuild` re-settles focus (rule 10) and the row stays where the thumb is.
        RowId::Errors => {
            let (e, u) = draft();
            unsafe { addr_of_mut!(DRAFT).write((!e, u)) };
            rebuild(sel);
            crate::ui::idle::invalidate();
        }
        RowId::Usage => {
            let (e, u) = draft();
            unsafe { addr_of_mut!(DRAFT).write((e, !u)) };
            rebuild(sel);
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
        RowId::ErrorsId => {
            unsafe {
                PREVIEW_KIND = PreviewKind::ErrorsId;
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

/// UP/DOWN — `route_screen`'s rules 1-4, with nothing screen-specific left in them. The action
/// band is BELOW the list on both modes, so UP leaves it and DOWN off the last row enters it;
/// first run opening focused on the band rather than the list is this route's one asymmetry, and
/// it lives in `enter_first_run_stage` rather than here.
pub(crate) fn on_updown(delta: i32) -> bool {
    if delete_alert().is_open() {
        return true;
    }
    if !menu_open() {
        return false;
    }
    let mut f = focus();
    let step = f.updown(shape(), delta);
    unsafe { addr_of_mut!(FOCUS).write(f) };
    apply_step(step)
}

/// LEFT/RIGHT — rules 5-9. First run's two answers are a ROW walked with these keys, and RIGHT off
/// the trailing one now continues into the reading list instead of stopping dead; Settings answers
/// them at all for the first time, which is what makes its Done reachable leftward and escapable
/// rightward.
pub(crate) fn on_left_right(delta: i32) -> bool {
    if delete_alert().is_open() {
        delete_alert().move_focus(delta);
        return true;
    }
    if !menu_open() {
        return false;
    }
    let mut f = focus();
    let s = shape();
    let step = if delta < 0 { f.left(s) } else { f.right(s) };
    unsafe { addr_of_mut!(FOCUS).write(f) };
    apply_step(step)
}

/// **Is everything this route draws parked where the pointer thinks it is?** Rule 11's timing
/// guard — see [`RoutePush::settled`]. Three transforms can be in flight here at once: the
/// popover's own entrance, the document push, and (first run) the Crash → Product stage push. The
/// recorded band rects and `list_frame()` are all FINAL-position, so a hit taken during any of them
/// acts on a control the person cannot see at those coordinates — most sharply just after BACK out
/// of a document, when the rows underneath become logically live while still travelling and nearly
/// transparent.
fn layers_settled() -> bool {
    pop().appear_settled()
        && unsafe { (*addr_of!(DOCUMENT_MORPH)).settled(preview_open()) }
        && (mode() == Mode::Settings
            || unsafe { (*addr_of!(STAGE_PUSH)).settled(stage() == Stage::Product) })
}

/// The frame the content column's table is drawn in — one function, so the pointer hit test and
/// the draw cannot disagree about where the rows are. Settings' list is SECTIONED (its first
/// label shares the narrative title's top anchor); first run's is a single headerless group.
fn list_frame() -> Rect {
    let l = RouteLayout::screen();
    if mode() == Mode::Settings {
        l.sectioned_table()
    } else {
        l.content
    }
}

/// **Rule 11, this screen's half.** Park focus under the pointer and report whether it parked on
/// anything, so a click in dead space cannot arm a press on whatever happened to be focused
/// already. Before this existed the whole route was pointer-DEAF — `app.rs` swallowed every click
/// over it and never routed a hover — which is the "Privacy & Data: neither hover nor click works"
/// half of the Magic Remote report.
pub(crate) fn pointer_focus(mx: f32, my: f32) -> bool {
    if !menu_open() || preview_open() || delete_alert().is_open() || !layers_settled() {
        return false;
    }
    let band = if mode() == Mode::FirstRun {
        unsafe { (*addr_of!(ANSWER_POP)).hit(mx, my) }
    } else {
        unsafe { (*addr_of!(DONE_POP)).hit(mx, my) }
    };
    if let Some(i) = band.filter(|_| action_visible()) {
        let mut f = focus();
        f.to_band(i);
        unsafe { addr_of_mut!(FOCUS).write(f) };
        sync_focus();
        crate::ui::idle::invalidate();
        return true;
    }
    if let Some(row) = list().hit_row(list_frame(), mx, my) {
        let mut f = focus();
        f.to_content();
        unsafe { addr_of_mut!(FOCUS).write(f) };
        list().sel = row;
        sync_focus();
        crate::ui::idle::invalidate();
        return true;
    }
    false
}

/// Pointer-down on a CONTROL FACE — an answer pill or Done: park focus and report the hit, so the
/// caller arms the tvOS press and spends it on the spring-back. A list row is not a control face
/// and is [`click_row`]'s.
pub(crate) fn press_at(mx: f32, my: f32) -> bool {
    let hit = layers_settled().then(|| band_hit(mx, my)).flatten();
    let ok = pointer_focus(mx, my) && focus_is_ctl() && hit.is_some();
    unsafe { addr_of_mut!(ARMED).write(ok.then(|| (PressFrom::Pointer, hit.unwrap()))) };
    ok
}

/// The delete-confirmation alert's own pointer-down, kept OUT of [`pointer_focus`] deliberately.
///
/// `DecisionAlert::press_at` measures its question through SDL2_ttf, which the host suite cannot
/// link (`ui/CLAUDE.md`'s host/device boundary) — so folding it into `pointer_focus` would make
/// every pointer test in this module a link error rather than an assertion. It is also the right
/// layering: the alert is a modal over the route, and `app.rs` already dispatches the exit alert's
/// pointer the same way.
pub(crate) fn alert_press_at(mx: f32, my: f32) -> bool {
    if !delete_alert().is_open() || !delete_alert().settled() {
        return false;
    }
    let hit = delete_alert().press_at(mx, my);
    unsafe { addr_of_mut!(ARMED_ALERT).write(hit.then(|| delete_alert().choice())) };
    hit
}

/// The alert's half of [`pointer_hold`]. It also PARKS — `DecisionAlert::press_at` sets the choice
/// it hit — which is the hover the alert never had: consent's ordinary pointer path refuses while
/// it is open, so moving the cursor over `Delete` used to leave the ring on `Cancel`.
pub(crate) fn alert_hold(mx: f32, my: f32) -> bool {
    if !delete_alert().is_open() || !delete_alert().settled() {
        return false;
    }
    let hit = delete_alert().press_at(mx, my);
    crate::ui::idle::invalidate();
    hit && crate::ui::press::is_live()
        && Some(delete_alert().choice()) == unsafe { *addr_of!(ARMED_ALERT) }
}

/// Is the delete confirmation the thing on screen? `app.rs` asks so it can route the pointer to the
/// alert's door rather than the route's.
pub(crate) fn alert_is_open() -> bool {
    delete_alert().is_open()
}

/// Pointer-down on a table ROW: park focus and REPORT it, leaving the activation to the caller's
/// `commit_consent` — the same function an OK key-down goes through. It has to be that one and not
/// a local `on_ok()`: the Delete-all-local-data answer sets a request that only `commit_consent`
/// harvests, so committing here would flip the switch and drop the sweep on the floor.
pub(crate) fn click_row(mx: f32, my: f32) -> bool {
    pointer_focus(mx, my) && !delete_alert().is_open() && focus().on_content()
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
    // Rule 10 on the frame clock as well as on the key: a draft edit, a stage change or a
    // rebuilt list can remove the control focus was on between one key and the next, and this is
    // the one place that runs whether or not anybody pressed anything.
    sync_focus();
    let band = focus().band_index();
    if mode() == Mode::Settings {
        table().update(dt, layout.sectioned_table().h);
        unsafe { (*addr_of_mut!(DONE_POP)).step(band.filter(|_| action_visible()), dt) };
    } else {
        // Both stages, not just the current one: `STAGE_PUSH` keeps the outgoing stage's own
        // table on screen (fading/sliding) for the whole transition, so both must stay warm.
        table_crash().update(dt, layout.content.h);
        table_product().update(dt, layout.content.h);
        unsafe { (*addr_of_mut!(ANSWER_POP)).step(band, dt) };
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
    if menu_open() {
        crate::ui::profile::phase("cs.page", draw_question);
    }
    // Self-gated on their OWN `visible()`, deliberately outside the `menu_open()` guard above: a
    // confirmed delete closes the rest of this screen at once (`close_delete_and_menu`'s outer
    // popover close) while the alert itself only DISMISSES, so its fade still has frames left to
    // draw once `menu_open()` has already gone false — over whatever the app shows next, exactly
    // like a dismissed `account_menu`/`item_menu` fading over its returning host. This alert is
    // sourced from the completed Privacy route, so its scrim belongs immediately before it here
    // rather than in the host-page closure (which sits under Settings' opaque wash).
    crate::ui::profile::phase("cs.alert", || {
        delete_alert().draw_scrim();
        delete_alert().draw(c"Delete all local data?", c"Cancel", c"Delete");
    });
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
/// Every control it draws also RECORDS its frame ([`crate::ui::route_screen::ActionRow::place`]),
/// which is the whole of what makes the band hoverable and clickable (rule 11): the hit rect and
/// the painted pill are then the same object rather than two derivations that agree by inspection.
/// The band that is not drawn clears its frames in `draw_question`, so a Done that an undo has just
/// removed leaves no live click target behind it.
fn draw_action_row(p: crate::ui::Painter, layout: RouteLayout) {
    if !action_visible() {
        return;
    }
    let band = focus().band_index();
    if mode() == Mode::Settings {
        let w = Button::pill_w(c"Done".as_ptr(), theme::size::BODY, false).min(layout.action.w);
        let r = Rect::new(layout.action.x, layout.action.y, w, layout.action.h);
        unsafe { (*addr_of_mut!(DONE_POP)).place(0, r) };
        Button::new(c"Done".as_ptr(), theme::size::BODY, r)
            .focused(band == Some(0))
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
    let palette = unsafe { (*addr_of!(GROUND)).palette() };
    for (i, (label, rect)) in [(share, share_r), (decline, decline_r)]
        .into_iter()
        .enumerate()
    {
        unsafe { (*addr_of_mut!(ANSWER_POP)).place(i, rect) };
        Button::new(label.as_ptr(), theme::size::BODY, rect)
            .focused(band == Some(i))
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
    // Rule 11: only a control DRAWN this frame may be hit. `draw_action_row` re-places whatever it
    // paints immediately below, so clearing first is what retires a Done an undo has removed and
    // the answer pills a pushed document has covered.
    unsafe {
        (*addr_of_mut!(DONE_POP)).clear();
        (*addr_of_mut!(ANSWER_POP)).clear();
    }
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
        table().draw(route_layer, list_frame());
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
            PreviewKind::ErrorsId => (
                DOC_TITLE_ERRORS_ID,
                "The random identifier attached to crash and error reports from this sign-in, and how to have those reports deleted.",
            ),
            PreviewKind::AnalyticsId => (
                DOC_TITLE_ANALYTICS_ID,
                "The random identifier attached to product analytics from this sign-in, and how to have those events deleted.",
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
            PreviewKind::ErrorsId => errors_id_document(),
            PreviewKind::AnalyticsId => analytics_id_document(),
            PreviewKind::Policy => privacy_policy().to_string(),
            PreviewKind::Crash => preview_crash(),
            PreviewKind::Usage => preview_usage(),
        };
        // A pushed document is PLAIN TEXT, so it hangs from the title's anchor rather than the
        // crumb's — `RouteLayout::document`, the family's one geometry rule for a text-only
        // content column. Its crumb is never absent here (it always names the question it was
        // opened from), but the flag is passed rather than assumed so the anchor cannot drift.
        reader().draw(dp, layout.document(true), None, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run whatever transform is in flight to rest. Rule 11 refuses a POSITIONAL hit until the
    /// layer it belongs to has arrived, so every pointer test here has to land its screen first.
    /// Spring the crate-global press all the way back to idle. A test that arms one owes this to
    /// the next test in the file: `press` is process-wide state, and `testlock::serial` orders the
    /// tests without cleaning up after them.
    fn end_press() {
        crate::ui::press::cancel();
        for i in 0..200 {
            crate::ui::press::tick(4 + i * 16, 0.016);
        }
    }

    fn settle() {
        for _ in 0..600 {
            update(1.0 / 60.0);
        }
    }

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
            let answered = consent::apply(&Consent::default(), e, u, || Some("id".into()));
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
            errors_id: Some("old-errors-id".into()),
        };
        assert!(should_show(&old, false));
        let current = consent::apply(&Consent::default(), true, false, || Some("new-id".into()));
        assert!(!should_show(&current, false));
    }

    /// **Each channel carries its own identifier now, so each is refused on its own mint.** This
    /// used to assert that crash reporting survived a failed mint because it was anonymous; it is
    /// not anonymous any more, and an opt-in that could not be given an identifier is recorded as
    /// off rather than sent bare. The transition itself is `consent::apply`'s and tested there;
    /// this pins that the screen goes through it with no second path.
    #[test]
    fn unavailable_randomness_refuses_the_channel_it_failed_for() {
        let answer = consent::apply(&Consent::default(), true, true, || None);
        assert!(answer.answered(), "the person is not asked again");
        assert!(!answer.errors && !answer.usage);
        assert!(answer.install_id.is_none() && answer.errors_id.is_none());
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

    /// **The two identifier documents each name only their own channel's identifier.** The
    /// documents read the stored decision, so this installs one with both ids and checks that
    /// neither page prints the other's value — the cross-linking the two-id design exists to
    /// prevent, checked at the one place a person actually reads the values.
    #[test]
    fn each_identifier_document_shows_only_its_own_identifier() {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        let errors_id = "e".repeat(32);
        let analytics_id = "a".repeat(32);
        let mut draws = 0;
        consent::install(consent::apply(&Consent::default(), true, true, || {
            draws += 1;
            Some(if draws == 1 {
                errors_id.clone()
            } else {
                analytics_id.clone()
            })
        }));
        let errors_doc = errors_id_document();
        let analytics_doc = analytics_id_document();
        assert!(errors_doc.contains(&errors_id) && !errors_doc.contains(&analytics_id));
        assert!(analytics_doc.contains(&analytics_id) && !analytics_doc.contains(&errors_id));
        assert!(errors_doc.contains(CONTACT_EMAIL) && analytics_doc.contains(CONTACT_EMAIL));

        consent::install(consent::apply(&Consent::default(), false, false, || None));
        assert!(errors_id_document().starts_with("NO CRASH REPORT ID"));
        assert!(analytics_id_document().starts_with("NO ANALYTICS ID"));
        if let Some(c) = saved {
            consent::install(c);
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
        assert!(
            text.contains("created only when crash reports are enabled"),
            "the crash intro explains its placeholder"
        );
        assert!(
            text.contains(crate::telemetry::native::PREVIEW_USER_ID),
            "and the crash-report id is shown as a placeholder"
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
                RowId::ErrorsId,
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
            focus().band_index().is_some(),
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
        assert_eq!(focus().band_index(), Some(0), "opens on Share");
        assert!(on_left_right(1));
        assert_eq!(focus().band_index(), Some(1));
        assert!(on_left_right(-1));
        assert_eq!(focus().band_index(), Some(0));
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
        assert!(focus().band_index().is_some());
        assert!(
            focus().band_index() == Some(0),
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
        assert!(focus().on_content());
        assert!(list().list_focused);
        list().sel = list().n_rows() - 1;
        assert!(on_updown(1));
        assert!(
            focus().band_index().is_some(),
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
        assert!(!focus().band_index().is_some(), "and not on a Done that is not there yet");
        close();
    }

    /// **Opening an already-open question must not restart it.** The consent question is asked
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

    /// **Toggling a switch leaves focus on the switch.** Reported: "focus immediately jumps to
    /// Done". It did — `on_ok`'s two switch arms wrote `ACTION_FOCUSED = true` on every flip — and
    /// the cost is not only surprise: it is the first half of the disappearing-focus bug below,
    /// and it made the row you were editing stop wearing its plate the moment you edited it.
    #[test]
    fn toggling_a_switch_keeps_focus_on_the_row_it_toggled() {
        let _g = crate::testlock::serial();
        open_settings(&Consent::default());
        list().sel = 0;
        assert!(on_ok(), "OK on the Crash reports switch");
        assert_eq!(draft(), (true, false), "…flips exactly that switch");
        assert!(
            focus().on_content(),
            "focus must stay on the row that was toggled, not jump to Done"
        );
        assert!(list().list_focused, "…and the list must still wear its plate");
        assert_eq!(list().sel, 0, "…on the row that was edited");
        close();
    }

    /// **The band and the list are one horizontal walk, both ways.** Reported: "from Done, Right
    /// does not return to the table, while Up strangely does". `on_left_right` only ever answered
    /// for first run, so in Settings mode LEFT and RIGHT were both dropped on the floor and the
    /// band was a rightward dead end (rules 5 and 7).
    #[test]
    fn left_reaches_the_action_band_and_right_comes_back_out_of_it() {
        let _g = crate::testlock::serial();
        open_settings(&Consent::default());
        unsafe { addr_of_mut!(DRAFT).write((true, false)) };
        rebuild(0);
        assert!(action_visible(), "Done is on screen once a value differs");
        assert!(on_left_right(-1), "LEFT from the list reaches the band");
        assert_eq!(focus().band_index(), Some(0));
        assert!(!list().list_focused, "…and only one accent capsule is drawn");
        assert!(on_left_right(1), "RIGHT from the band returns to the list");
        assert!(focus().on_content());
        assert!(list().list_focused);
        close();
    }

    /// **No edit may leave focus on nothing.** Reported: "toggling the value back can cause focus
    /// to disappear completely". Exactly so — the flip parked focus on Done, and toggling back made
    /// the draft match the stored answer again, which REMOVES Done. Nothing re-seated focus, so the
    /// screen had no ring anywhere while the keys still moved an invisible selection (rule 10).
    #[test]
    fn toggling_a_value_back_never_leaves_focus_on_nothing() {
        let _g = crate::testlock::serial();
        open_settings(&Consent::default());
        list().sel = 0;
        on_ok(); // Crash reports -> On, so Done appears
        assert!(action_visible());
        assert!(on_updown(-1) || true);
        on_ok(); // …and back Off again, so Done goes away under whatever was focused
        assert!(!action_visible(), "the draft matches the stored answer again");
        assert!(
            focus().on_content() && list().list_focused,
            "with no Done left to hold it, focus must be back on the list rather than nowhere"
        );
        close();
    }

    /// **First run's two answers are not a rightward dead end.** Reported against Share Crash
    /// Reports: "focus cannot navigate correctly from the bottom buttons back to the options on the
    /// right". LEFT/RIGHT walked the two answers and stopped; only UP could reach the reading list
    /// (rule 7).
    #[test]
    fn right_off_the_trailing_answer_reaches_the_reading_list() {
        let _g = crate::testlock::serial();
        open(&Consent::default());
        assert_eq!(focus().band_index(), Some(0), "first run opens on the answers");
        assert!(on_left_right(1), "RIGHT walks to the second answer");
        assert_eq!(focus().band_index(), Some(1));
        assert!(on_left_right(1), "…and RIGHT off the trailing answer reaches the list");
        assert!(focus().on_content());
        assert!(list().list_focused);
        // …and LEFT walks back in, trailing control first, so the round trip is exact.
        assert!(on_left_right(-1));
        assert_eq!(focus().band_index(), Some(1));
        close();
    }

    /// **Rule 11 on the route that had no pointer at all.** Hover parks the answer under the
    /// cursor; dead space parks nothing and leaves focus where it was. The two pill frames are
    /// `place`d here exactly as `draw_action_row` places them, since a host test cannot draw.
    #[test]
    fn hover_parks_an_answer_and_dead_space_parks_nothing() {
        let _g = crate::testlock::serial();
        open(&Consent::default());
        settle();
        let (share_r, decline_r) = RouteLayout::screen().action_pair(240.0, 200.0);
        unsafe {
            (*addr_of_mut!(ANSWER_POP)).place(0, share_r);
            (*addr_of_mut!(ANSWER_POP)).place(1, decline_r);
        }
        assert!(pointer_focus(decline_r.x + 10.0, decline_r.y + 10.0));
        assert_eq!(focus().band_index(), Some(1), "hover parks the pill under the cursor");
        assert!(
            !pointer_focus(4.0, 4.0),
            "the top-left corner is not a control and not a row"
        );
        assert_eq!(
            focus().band_index(),
            Some(1),
            "…and parking nothing must not move the ring"
        );
        assert!(pointer_focus(share_r.x + 10.0, share_r.y + 10.0));
        assert_eq!(focus().band_index(), Some(0));
        // …and a control face is `press_at`'s, not `click_row`'s, so a pill never commits flat on
        // the button-down.
        assert!(press_at(share_r.x + 10.0, share_r.y + 10.0));
        assert!(!click_row(share_r.x + 10.0, share_r.y + 10.0));
        assert_eq!(draft(), (false, false), "and hovering or arming answers nothing");
        close();
    }

    /// **An armed press is bound to the control it was armed on.** `ui::press`'s model is that
    /// focus cannot move mid-press, which the nav keys pay by cancelling; `app.rs` pays it for
    /// hover through [`pointer_hold`]. On THIS screen the cost of getting it wrong is a recorded
    /// consent decision: a pointer-down on `Share reports` plus ordinary Magic Remote jitter inside
    /// the ~210 ms commit window would answer the other way, or — the harder half — slide off
    /// every control and answer the ORIGINAL way anyway.
    #[test]
    fn an_armed_press_survives_jitter_but_not_leaving_the_control() {
        let _g = crate::testlock::serial();
        open(&Consent::default());
        settle();
        let (share_r, decline_r) = RouteLayout::screen().action_pair(240.0, 200.0);
        unsafe {
            (*addr_of_mut!(ANSWER_POP)).place(0, share_r);
            (*addr_of_mut!(ANSWER_POP)).place(1, decline_r);
        }
        // `press_at` records what the press is FOR; `begin_ctl` is the press itself, and `armed`
        // deliberately reads through it — so a test that omits it exercises the UN-armed path,
        // where every hover is held by definition.
        assert!(press_at(share_r.x + 10.0, share_r.y + 10.0), "arm on Share");
        crate::ui::press::begin_ctl(1);
        assert!(
            pointer_hold(share_r.x + 12.0, share_r.y + 12.0),
            "two pixels of jitter is still the same pill, so a real click must still commit"
        );
        assert!(
            !pointer_hold(decline_r.x + 10.0, decline_r.y + 10.0),
            "the other answer is a different control, and an armed press must not follow it"
        );
        // **The case the first version of this guard missed**: sliding off EVERY control. A miss
        // leaves focus exactly where it was, so a before/after comparison of the FOCUSED stop saw
        // no change and the press still committed the answer the pointer had left.
        assert!(press_at(share_r.x + 10.0, share_r.y + 10.0), "re-arm on Share");
        crate::ui::press::begin_ctl(2);
        assert!(
            !pointer_hold(4.0, 4.0),
            "dead space is not the control this press was armed on"
        );
        // …but a KEY-origin press is bound to the FOCUS STOP rather than to a rect, so the same
        // hover across dead space — which moves no focus — leaves a key the user is still holding
        // alone. Same press, same coordinates, opposite answer, and that is the whole reason
        // `PressFrom` exists (Codex review, 2026-09-04).
        focus().to_band(0);
        arm_key();
        crate::ui::press::begin_ctl(3);
        assert!(
            pointer_hold(4.0, 4.0),
            "dead space parks no focus, so it cannot retract a press the pointer never made"
        );
        assert_eq!(draft(), (false, false), "and none of this answers anything");
        end_press();
        close();
    }

    /// **A positional hit is refused while its layer is still travelling.** Codex review,
    /// 2026-09-04: a key acts on the LOGICAL state and is right to — `DOCUMENT_OPEN` goes false the
    /// instant BACK is pressed — but the rects a click is tested against are FINAL-position, so
    /// during the reverse push the rows underneath were accepting clicks while still translated and
    /// nearly transparent.
    #[test]
    fn a_pointer_hit_waits_for_its_layer_to_arrive() {
        let _g = crate::testlock::serial();
        open_settings(&Consent::default());
        // The popover's own entrance is a transform too, so nothing is hittable until it lands.
        assert!(!layers_settled(), "the modal is still appearing");
        let f = list_frame();
        assert!(
            !pointer_focus(f.x + 40.0, f.y + 60.0),
            "…and a hit taken now would act on rows that are not under the pointer yet"
        );
        for _ in 0..600 {
            update(1.0 / 60.0);
        }
        assert!(layers_settled(), "settled once the entrance finishes");
        let mut parked = false;
        for i in 0..80 {
            parked |= pointer_focus(f.x + 40.0, f.y + 10.0 + i as f32 * 12.0);
        }
        assert!(parked, "…and then the rows are live again");
        close();
    }

    /// A row IS `click_row`'s, and it reports rather than committing — the delete-everything
    /// outcome is `app.rs`'s `commit_consent` to harvest, so a local activation here would flip a
    /// switch and drop the sweep.
    #[test]
    fn a_row_click_parks_the_row_and_leaves_the_commit_to_the_caller() {
        let _g = crate::testlock::serial();
        open_settings(&Consent::default());
        settle();
        let f = list_frame();
        // Scan the column rather than guessing a row's y: the table owns its own header band and
        // paddings, and a literal here would be a transcription of them.
        let mut parked = std::collections::BTreeSet::new();
        let mut a_row_y = None;
        for i in 0..80 {
            let y = f.y + 10.0 + i as f32 * 12.0;
            if pointer_focus(f.x + 40.0, y) {
                parked.insert(list().sel);
                a_row_y.get_or_insert(y);
            }
        }
        assert!(
            parked.len() >= 2,
            "hover must park different rows at different heights, parked {parked:?}"
        );
        let y = a_row_y.expect("some y in the content column is over a row");
        assert!(click_row(f.x + 40.0, y), "a row is a click target");
        assert_eq!(draft(), (false, false), "…and reports rather than committing");
        assert!(!click_row(f.x + 40.0, f.y - 400.0), "dead space is not");
        close();
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

    /// Run the delete alert's appear spring to rest before dismissing it. `is_open()` (and so the
    /// input gate every exit path below checks) goes true the instant `open_inner` runs, so a rapid
    /// enough press really could answer mid-ramp — this is not modelling an invariant the input
    /// system enforces. It exists so `visible()` has real fade to lose: dismissing at `appear ==
    /// 0.0` would still make `visible()` true immediately, because `dismiss()` sets `closing`
    /// unconditionally — but the very next `update()` would find the spring already at rest and
    /// clear `closing` on the spot, ending the panel in one frame with no fade actually drawn. That
    /// passing the same assertion for the wrong reason is exactly the false confidence this test
    /// exists to rule out.
    fn settle_delete_alert() {
        for _ in 0..240 {
            delete_alert().update(1.0 / 60.0);
        }
    }

    /// **Reported: confirming Delete Local Data has no dismissal animation.** The alert plays a
    /// real appear animation on open; `on_ok`'s Destructive arm used to jump straight to the
    /// module's own `close()`, which hides the alert (and the rest of the screen) INSTANTLY —
    /// the one exit from this alert that skipped the fade every other one plays. Observed red
    /// against the code this replaces: temporarily putting `close()` back in the Destructive arm
    /// (in place of `close_delete_and_menu(true)`), rerunning this exact test, and watching it fail
    /// at the `delete_alert().visible()` assertion below with "…but the panel must still be DRAWN
    /// — a fade in flight, not an instant vanish" — a real revert-and-rerun, not a simulated one.
    #[test]
    fn confirming_delete_dismisses_the_alert_rather_than_closing_it_instantly() {
        let _g = crate::testlock::serial();
        delete_alert().open_with_body(DELETE_SCOPE);
        delete_alert().set_choice(AlertChoice::Destructive);
        settle_delete_alert();
        assert!(on_ok(), "OK on an open delete alert must be handled here");
        assert!(
            !delete_alert().is_open(),
            "input modality still ends on the press frame, same as every other popover"
        );
        assert!(
            delete_alert().visible(),
            "…but the panel must still be DRAWN — a fade in flight, not an instant vanish"
        );
        assert!(
            take_delete_request(),
            "the destructive answer must still be recorded for the caller to act on"
        );
        // Cancel gets the same treatment — dismiss, not close.
        delete_alert().open_with_body(DELETE_SCOPE);
        settle_delete_alert();
        assert!(on_ok(), "OK on Cancel must also be handled here");
        assert!(!delete_alert().is_open());
        assert!(delete_alert().visible(), "Cancel fades too");
        assert!(!take_delete_request(), "declining must never set the delete request");
        // …and BACK.
        delete_alert().open_with_body(DELETE_SCOPE);
        settle_delete_alert();
        assert!(on_back());
        assert!(!delete_alert().is_open());
        assert!(delete_alert().visible(), "BACK fades too");

        // Drain the fade so this test leaves the shared alert at rest for whichever test runs next.
        for _ in 0..240 {
            delete_alert().update(1.0 / 60.0);
        }
        assert!(!delete_alert().visible());
    }
}
