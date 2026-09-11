//! Legal and About routes on the Settings modal's frozen full-screen ground.

use crate::ui::consts::SCR_W;
use crate::ui::document_reader::DocumentReader;
use crate::ui::popover::Popover;
use crate::ui::route_screen::{RouteFocus, RouteLayout, RoutePush, RouteShape, RouteStep};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::theme;
use std::ptr::{addr_of, addr_of_mut};

/// The index's own title, and — one push deeper — the crumb a document names.  One constant, so
/// the two can never disagree about what the screen behind you is called.
const INDEX_TITLE: &str = "Legal notices";
/// Where BACK goes from this family's two entrances. Both are opened from the Settings root.
const CRUMB_SETTINGS: &str = "Settings";
/// **The one contact address the application prints.** Every document that offers a way to reach a
/// human must print THIS and nothing else.
///
/// `every_document_prints_only_the_one_contact_address` enforces it by scanning for stray `@`s
/// rather than by trusting the next editor to grep — but it iterates `Page::ALL`, so its reach is
/// THIS module's documents. `ui::consent`'s Analytics ID page is covered by construction instead:
/// it imports this constant rather than holding an address of its own.
///
/// It is a `&str` beside literals rather than interpolated into them because `concat!` takes
/// literals only and these documents are `&'static str` the reader borrows; the test is what makes
/// the duplication safe, the same bargain the two privacy-policy doors struck.
pub(crate) const CONTACT_EMAIL: &str = "support@plxnative.com";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Page {
    Privacy,
    OpenSource,
    Ffmpeg,
    Source,
    Trademarks,
    Contact,
}
impl Page {
    pub(crate) const ALL: [Self; 6] = [
        Self::Privacy,
        Self::OpenSource,
        Self::Ffmpeg,
        Self::Source,
        Self::Trademarks,
        Self::Contact,
    ];
    fn title(self) -> &'static str {
        match self {
            Self::Privacy => "Privacy policy",
            Self::OpenSource => "Open-source licences",
            Self::Ffmpeg => "FFmpeg & source offer",
            Self::Source => "PlxNative source code",
            Self::Trademarks => "Trademarks & non-affiliation",
            Self::Contact => "Privacy & security contact",
        }
    }
    fn subtitle(self) -> &'static str {
        match self {
            Self::Privacy => "How PlxNative handles local data and optional reports.",
            Self::OpenSource => "Components, copyright holders and licence texts.",
            Self::Ffmpeg => "LGPL notice, replaceability and corresponding source.",
            Self::Source => "Project source, build scripts and release materials.",
            Self::Trademarks => "Independent-client status and trademark attribution.",
            Self::Contact => "How to ask a privacy question or report a vulnerability.",
        }
    }
    /// The document itself. `pub(crate)` because this module is the SOURCE OF TRUTH for the legal
    /// texts and one of them has a second door: `ui::consent`'s Information section offers the
    /// privacy policy beside the toggles it informs, and reads it from here rather than keeping a
    /// second literal that can drift (it had already drifted — see that module's
    /// `both_privacy_policy_doors_open_the_same_document`).
    pub(crate) fn body(self) -> &'static str {
        match self {
            Self::Privacy => PRIVACY,
            Self::OpenSource => OPEN_SOURCE,
            Self::Ffmpeg => FFMPEG,
            Self::Source => SOURCE,
            Self::Trademarks => TRADEMARKS,
            Self::Contact => CONTACT,
        }
    }
}

const PRIVACY: &str = "RESPONSIBLE FOR PLXNATIVE DATA\n\nGleb Linnik is responsible only for data PlxNative stores locally and for optional reports you choose to share.\n\nPLEX SERVICES\n\nPlxNative is an independent client for Plex. To sign you in, discover servers and provide Plex account features, the app communicates directly with Plex services. Plex processes information received by those services under Plex’s own Privacy Policy. PlxNative’s developer does not receive that information.\n\nPlex Privacy Policy: https://www.plex.tv/about/privacy-legal/\n\nPLEX MEDIA SERVERS\n\nTo browse and play media, update watch progress and use server features, PlxNative communicates directly with the Plex Media Servers you select. Those requests are handled by the selected server and its operator. PlxNative’s developer does not receive them.\n\nON THIS TELEVISION\n\nPlxNative stores your Plex account token and a separate token for each server you use, the addresses and identifiers of those servers, the profile you selected together with the profile names and pictures on your account, your Home library choices, your recent searches, your playback quality preference, and a small rotating local log. It also stores your answers to the two optional-reporting questions, the random Crash report ID if you turned crash reports on, the random Analytics ID if you turned product analytics on, any report waiting to be sent, and a marker recording how much of the crash log has already been read. It keeps no bookmark of its own for where you stopped watching: playback position is held by your Plex Media Server. Delete all local data in Settings signs out immediately, queues removal of PlxNative data from this television, and reports if that cleanup is incomplete.\n\nYour sign-in is protected with this television's own key service when one is available and this install has shown it can be trusted: the app checks, on a later launch, that the television's key service can still open something it sealed before, and only after that check has passed does it seal your actual sign-in with it — until then the sign-in is kept in an owner-only file that only PlxNative can read. Two small, content-only markers on disk record the outcome of that check: `secure-storage.proven` records that the key service has been shown to work on this install, `secure-storage.refused` that it has been shown not to. Neither carries key material. If the saved sign-in file is ever found writable by other apps on this television, its contents are not trusted or used: it is set aside unread as `<id>-auth.json.untrusted` (or `state/auth.json.untrusted`) beside itself, still readable only by PlxNative, and signing out or Delete all local data attempts to remove it and reports if cleanup is incomplete.\n\nOPTIONAL CRASH REPORTS\n\nIf enabled, technical crash details are sent to Sentry in Germany. They can include the signal, code addresses, thread information and device compatibility details. Each report carries a random Crash report ID created on this television when you turned crash reports on, so that repeated crashes under one Crash report ID are counted once rather than once each. It is not derived from your Plex account, your television or anything about you, and it is never sent with product analytics. Settings shows it as your Crash report ID while crash reports are on. The same choice also covers a handled sign-in error report when a sign-in attempt fails: which stage failed, a coarse class of what plex.tv's last answer actually was, that exact HTTP status or curl return code as a bare number, bucketed counts and durations, and how your sign-in is stored on this television right now. It contains no PIN, sign-in code, token, account, URL, hostname or address. The same choice also covers a handled storage error report when this television's attempt to seal or open your saved sign-in fails, or the file itself cannot be written at all: which step failed, the numeric error code the key service replied with, how the session is protected right now, and whether this install has already recorded its key service as refused. When it is known, it also says whether the device key that attempt used already existed or was newly created, and always says how that attempt identified itself to the key service, as two Yes/No facts: whether it used an application identity, and whether it instead used a fixed name on the system bus — never both, and No to both on a television whose system bus grants neither. Separately, it says which identity protected the saved sign-in the report is about, as one of app_id, named, anonymous, or none where the report is about nothing protected at all. It contains no key material, ciphertext, plaintext or file path. The same storage error report can also carry a compact candidate-location summary of which of this television's candidate sign-in locations were checked and how each one went. It also covers a fresh sign-in whose file could not be kept the way this television decided to keep it: what the save actually did and, when it left an existing file alone rather than overwriting it, why; it may carry the same save-outcome and candidate-location facts — each outcome one closed word, including one that says only that the file was there and its contents were not recognised at all — built only from closed words and small numbers and never a file path. It is sent only once you have answered the crash-reports question Yes; a report found before that question is answered waits in memory, dated to when it actually happened, for the rest of that one launch only, and is discarded if you answer No or if the app closes before you answer. Separately, whether or not crash reporting is on, you can request a diagnostic report from Details on the sign-in screen. The screen can also offer a one-off report about a specific sign-in problem. Opening Details sends nothing: you must confirm Send report. Working QR polling is not labelled as a connection failure merely because you requested diagnostics. The report distinguishes the original startup read from a later fresh sign-in save: candidate categories and rejection reasons; credential-presence and local-boot booleans; key-service stages and numeric errors; registration and sealed-key identity categories; save outcomes and disk readback; and per-candidate write operations, numeric OS errors, policy rejections and directory-sync durability warnings. It also includes app/webOS versions and hardware-class information, never file contents, paths, account names, credentials, PINs, sign-in codes or network addresses. It carries no identifier that persists between reports or identifies you or this television — not the Crash report ID, not the Analytics ID. A random one-off Report ID identifies only that event and is shown in Details so you can quote it. Sending it does not enable either optional reporting category or store an account-wide consent decision. Reports normally queue on disk for background delivery. If the queue cannot be written, one bounded background send is attempted directly from memory. Queued does not mean received; a direct request is marked sent only after a successful server response. If it fails, you can retry explicitly. An in-memory request cannot survive closing the app.\n\nOPTIONAL PRODUCT ANALYTICS\n\nIf enabled, screen and feature events and broad sign-in and playback outcomes are sent to PostHog in Germany with a random Analytics ID created when you turned product analytics on, and every event carries how the saved sign-in is stored on this television. Settings shows that identifier as your Analytics ID while product analytics is on.\n\nNEVER INCLUDED\n\nTitles, Plex accounts, searches, server names or addresses, tokens, subtitle text, key material, ciphertext, plaintext, file paths, and exact viewing history are not included in either optional report type. Both choices are independent and can be changed at any time in Settings.\n\nRETENTION\n\nDifferent things here have different lifetimes, so this is stated for each. Signing out immediately clears your in-memory sign-in, server access and reporting gates, then queues durable cleared records and removal of the saved sign-in, registered servers and tokens. Your answers to the two optional-reporting questions, and the Crash report ID and Analytics ID if they exist, belong to that sign-in: signing out removes them with it, and whoever signs in next is asked afresh. Switching between the profiles of one Plex account is not a sign-out and keeps them. A report waiting to be sent is deleted once it is sent. Switching a category off immediately closes its gate and deletes its identifier, then queues removal of that category's reports; one report the sender had already picked up at that moment may still be sent, and no further report of that category is picked up after it. Signing out, or Delete all local data, immediately closes both reporting gates and clears pending in-memory reports, then orders durable cleared records and removal of everything queued on disk, including one-off reports, before a new sign-in is permitted. Cleanup failure is shown separately; obsolete local material may remain, but closed gates do not permit it to be sent. Neither action can retract a request already transmitted. The local log rotates, so its oldest lines are discarded continuously. Delete all local data queues removal of all of it and reports incomplete cleanup. A report that has already been sent is held by the service that received it, under that service’s own retention schedule; write to the contact below to ask what those periods currently are.\n\nYOUR CHOICES AND HOW TO ASK\n\nBoth optional reports are off until you turn them on, and either can be turned off again at any time in Settings. When this notice changes, whether you are asked again depends on what changed: a wording-only revision describing the same data and the same purpose is never a new question; a new field that still fits the data and purpose you already read about is checked against the answer you already gave rather than opening one; a wider purpose, or data materially different from what you were told, is a new question for that change alone, asked before PlxNative starts collecting it, and it names only the category that actually grew. Declining an expansion is not the same as switching a category off: it keeps what you already agreed to running exactly as it was, sends nothing of the new part, and does not ask again until it grows further still — turning a category off entirely stays a separate action, any time, in Settings. The record of what you accepted, and what you last declined, is kept, so a later expansion is checked against the real answer you gave, not a guess. Delete all local data immediately closes access to what PlxNative stored on this television, queues its removal, and reports incomplete cleanup; it does not reach anything already sent. To ask what crash reports or product analytics hold for your installation, or to have them deleted, write to the contact below and quote your Crash report ID or Analytics ID from Settings. For a particular one-off report, quote the one-off Report ID shown in Details. That random event identifier does not link separate reports to you or your television. Turning a category off deletes its identifier from this television, and so does signing out; reports already sent keep the old one, so copy it down first if you intend to ask for their deletion.\n\nWHERE DATA IS PROCESSED\n\nOptional crash reports are processed by Sentry in Germany and optional product analytics by PostHog in Germany. Plex processes what its own services receive under Plex’s Privacy Policy. A Plex Media Server you connect to may be located anywhere and is operated by whoever runs it, not by PlxNative’s developer.\n\nUNINSTALLING\n\nRemoving PlxNative removes the packaged app directory, including fallback files `state/auth.json` and `state/telemetry.json`. External session and reporting-decision files can survive uninstall because webOS gives an application no way to run code as it is removed. Use Delete all local data and resolve any reported cleanup failure BEFORE uninstalling if you want those external files removed too.\n\nCONTACT\n\nPrivacy questions: support@plxnative.com";
const OPEN_SOURCE: &str = "PlxNative is free software under the MIT Licence. Copyright (c) 2026 Gleb Linnik.\n\nThe application package includes THIRD-PARTY-NOTICES.md, the complete licence texts and font notices. Included projects include libcurl, SDL2, SDL2_ttf, nanosvg, zlib, jsmpeg, Inter, Noto Sans CJK, Feather, Heroicons, Material Icons and the Rust crates used by this build.";
const FFMPEG: &str = "This software uses libraries from the FFmpeg project under the LGPLv2.1. FFmpeg is copyright (c) the FFmpeg developers; PlxNative does not own FFmpeg.\n\nThe FFmpeg libraries are unmodified and loaded dynamically, and may be replaced with an interface-compatible build. The complete corresponding FFmpeg 9.0 source, exact configure line and build script are published with every PlxNative release.";
const SOURCE: &str = "PlxNative source code, release materials and build scripts are published at:\n\ngithub.com/GLinnik21/plx-native\n\nRelease source packages include the corresponding FFmpeg source and the script used to build it.";
const TRADEMARKS: &str = "Plex, the Plex logo and Plex Media Server are trademarks of Plex, Inc.\n\nLG and webOS are trademarks of LG Electronics Inc.\n\nPlxNative is an independent, unofficial application. It is not produced by, endorsed by, or affiliated with Plex, Inc. or LG Electronics Inc.";
const CONTACT: &str = "Privacy questions may be sent to support@plxnative.com.\n\nSecurity vulnerabilities may be reported privately through GitHub Security Advisories for GLinnik21/plx-native. Please do not include Plex tokens, server addresses or personal media information in a report.";
/// The one page here that carries a NUMBER, and so the one that can go stale on its own.
///
/// It was a literal — `PlxNative 0.5.0`, hand-typed, and not on `ci/bump-version.py`'s list of
/// files to bump, so it was already the only surface in the app that could disagree with every
/// other. It is composed from the same `PLX_VERSION` the diagnostics panel and `X-Plex-Version`
/// report, so a release build says `0.5.0` here and a developer build says `0.6.0-dev`: this is
/// a screen a user is asked to read out in a bug report, and it must name the binary they are
/// running rather than the last thing that was published. `PLX_BUILD_SHA` (`build.rs`'s
/// `emit_build_sha`) goes further than the version alone can: every trunk commit between two
/// releases reports the identical `X.Y.0-dev`, so the commit is what actually distinguishes one
/// developer build from the next.
///
/// **No `VERSION`/`DEVELOPER`/`LICENCE`/`PROJECT` section labels** — every other document here
/// uses that ALL-CAPS-line-as-heading idiom (`DocumentReader::rebuild_layout` bolds a fully
/// uppercase line), which reads right over several paragraphs of prose and wrong over four lines
/// that are each already self-explanatory; a label per line was closer to a form than to
/// something meant to be read.
///
/// **No leading `PlxNative` line, either** (owner correction, 2026-09-04) — the route's own
/// narrative title already draws "About PlxNative" directly above this body, so repeating the name
/// as the first body line duplicated it rather than adding anything.
///
/// `concat!` rather than a `format!` at draw time: the whole page is a `&'static str` the reader
/// borrows, and `env!` is a literal at expansion.
const ABOUT: &str = concat!(
    "Version ",
    env!("PLX_VERSION"),
    "\nBuild ",
    env!("PLX_BUILD_SHA"),
    "\n\nDeveloped by Gleb Linnik\n\u{00A9} 2026 Gleb Linnik",
    "\n\nOpen source under the MIT License\ngithub.com/GLinnik21/plx-native",
    "\n\nPlxNative is an independent, unofficial application. It is not produced by, endorsed by, or affiliated with Plex, Inc. or LG Electronics Inc."
);

static mut POP: Popover = Popover::new();
static mut TABLE: TableView = TableView::new();
static mut PAGE: Page = Page::Privacy;
static mut DETAIL: bool = false;
static mut ABOUT_MODE: bool = false;
static mut ROUTE_PUSH: RoutePush = RoutePush::new();
static mut READER: DocumentReader = DocumentReader::new();
fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}
fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}
fn reader() -> &'static mut DocumentReader {
    unsafe { &mut *addr_of_mut!(READER) }
}
pub(crate) fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}
fn detail() -> bool {
    unsafe { *addr_of!(DETAIL) }
}

fn build() {
    let mut s = Section::new("Legal");
    for page in Page::ALL {
        s = s.row(Row::new(page.title()).detail(page.subtitle()).chevron(true));
    }
    table().compact = false;
    table().header_ink = theme::TEXT_READING;
    table().set_sections(vec![s], 0, false);
}
pub(crate) fn open() {
    build();
    unsafe {
        PAGE = Page::Privacy;
        DETAIL = false;
        ABOUT_MODE = false;
        (*addr_of_mut!(ROUTE_PUSH)).jump(false);
    }
    reader().reset();
    table().list_focused = true;
    pop().open();
    crate::ui::idle::invalidate();
}
pub(crate) fn open_about() {
    unsafe {
        ABOUT_MODE = true;
        DETAIL = true;
        (*addr_of_mut!(ROUTE_PUSH)).jump(true);
    }
    reader().reset();
    pop().open();
    crate::ui::idle::invalidate();
}
pub(crate) fn close() {
    pop().dismiss();
    crate::ui::idle::invalidate();
}
pub(crate) fn on_back() -> bool {
    if !is_open() {
        return false;
    }
    if detail() && unsafe { !ABOUT_MODE } {
        unsafe {
            DETAIL = false;
        }
        reader().reset();
        table().list_focused = true;
        crate::ui::idle::invalidate();
    } else {
        close();
    }
    true
}
pub(crate) fn on_ok() -> bool {
    if !is_open() {
        return false;
    }
    if !detail() {
        let i = table().sel.clamp(0, Page::ALL.len() as i32 - 1) as usize;
        unsafe {
            PAGE = Page::ALL[i];
            DETAIL = true;
        }
        reader().reset();
        table().list_focused = false;
        crate::ui::idle::invalidate();
    }
    true
}
/// This route's shape, as `ui::route_screen`'s shared rules see it. **Neither state has an action
/// band**, which is what makes rule 9 apply here in full: LEFT is BACK from the index as well as
/// from inside a document, and that cannot discard anything — because THIS screen edits nothing at
/// all, not because it is bandless. The two are not the same claim: the Home editor is bandless
/// exactly when it believes it is clean, and "clean" there is a comparison of its draft against
/// what it entered with (`onboard::dirty`), which is why `uncommitted` is a field a screen
/// answers rather than something the model reads off `band` (Codex review, 2026-09-04).
fn shape() -> RouteShape {
    if detail() {
        return RouteShape::document();
    }
    let t = table();
    RouteShape {
        band: 0,
        rows: t.n_rows() > 0,
        at_last_row: t.at_last_row(),
        opens: t.row_opens(t.sel),
        // Every document here is a constant and the index edits nothing, so there is never
        // anything for BACK to discard — rule 9 applies without its guard ever engaging.
        uncommitted: false,
    }
}

/// The shared rules, with this screen's own two content columns behind them. Focus is a fresh
/// [`RouteFocus::content`] every time rather than a static, and that is not a shortcut: with no
/// band there is exactly one focus stop, so a stored one could only ever hold the value it was
/// initialised with.
fn step(delta_updown: Option<i32>, delta_leftright: Option<i32>) -> RouteStep {
    let mut f = RouteFocus::content();
    let s = shape();
    match (delta_updown, delta_leftright) {
        (Some(d), _) => f.updown(s, d),
        (_, Some(d)) if d < 0 => f.left(s),
        (_, Some(_)) => f.right(s),
        _ => RouteStep::Wall,
    }
}

fn apply(st: RouteStep) -> bool {
    match st {
        RouteStep::Scroll(delta) => {
            if detail() {
                reader().move_by(delta);
            } else {
                table().move_sel(delta);
            }
            crate::ui::idle::invalidate();
        }
        RouteStep::Enter => {
            on_ok();
        }
        RouteStep::Back => {
            on_back();
        }
        RouteStep::Wall | RouteStep::Moved => {}
    }
    true
}

pub(crate) fn on_updown(delta: i32) -> bool {
    if !is_open() {
        return false;
    }
    apply(step(Some(delta), None))
}
pub(crate) fn on_left_right(delta: i32) -> bool {
    if !is_open() {
        return false;
    }
    apply(step(None, Some(delta)))
}

/// **Rule 11.** The index's rows are hoverable and clickable like every other list in the family;
/// a document has no target at all, so a click over one parks nothing rather than closing it by
/// accident. Before this, `app.rs` swallowed every click over the whole Legal route.
pub(crate) fn pointer_focus(mx: f32, my: f32) -> bool {
    // …and only while the index is parked where the pointer thinks it is: a positional hit taken
    // during the entrance or the reverse push out of a document would act on rows that are still
    // travelling and nearly transparent (`RoutePush::settled`).
    if !is_open() || detail() || !pop().appear_settled() {
        return false;
    }
    if !unsafe { (*addr_of!(ROUTE_PUSH)).settled(false) } {
        return false;
    }
    if let Some(row) = table().hit_row(RouteLayout::screen().sectioned_table(), mx, my) {
        table().sel = row;
        table().list_focused = true;
        crate::ui::idle::invalidate();
        return true;
    }
    false
}

pub(crate) fn click(mx: f32, my: f32) -> bool {
    pointer_focus(mx, my) && on_ok()
}
pub(crate) fn update(dt: f32) {
    pop().update(dt);
    unsafe {
        (*addr_of_mut!(ROUTE_PUSH)).update(DETAIL, dt);
    }
    reader().update(dt);
    table().update(dt, RouteLayout::screen().sectioned_table().h);
}
pub(crate) fn draw_scrim() {
    if pop().visible() {
        pop().scrim(theme::alert::SCRIM_A);
    }
}
pub(crate) fn draw() {
    if !pop().visible() {
        return;
    }
    let pop = pop();
    let a = pop.appear();
    let p = pop
        .content_painter(0.0)
        .alpha(a)
        .translate(SCR_W as f32 * (1.0 - a), 0.0);
    let layout = RouteLayout::screen();
    if unsafe { ABOUT_MODE } {
        // About is this same document route entered directly from the Settings index, so its
        // crumb names Settings rather than the Legal index it never passed through.
        layout.draw_narrative(
            p,
            Some(CRUMB_SETTINGS),
            "About PlxNative",
            "A native media client built for LG webOS.",
            theme::size::LABEL,
        );
        reader().draw(p, layout.document(true), None, ABOUT);
        return;
    }
    let t = unsafe { (*addr_of!(ROUTE_PUSH)).amount() };
    if t < 0.999 {
        let index = unsafe { (*addr_of!(ROUTE_PUSH)).parent(p) };
        layout.draw_narrative(
            index,
            Some(CRUMB_SETTINGS),
            INDEX_TITLE,
            "Read the notices that apply to this build, its open-source components and its relationship with Plex and LG.",
            theme::size::LABEL,
        );
        table().draw(index, layout.sectioned_table());
    }
    if t > 0.01 {
        let page = unsafe { PAGE };
        let document = unsafe { (*addr_of!(ROUTE_PUSH)).child(p) };
        // The pushed document's crumb names the index it came from, which is the whole reason
        // this family can be three deep without anyone having to remember how they got here.
        layout.draw_narrative(
            document,
            Some(INDEX_TITLE),
            page.title(),
            page.subtitle(),
            theme::size::LABEL,
        );
        reader().draw(document, layout.document(true), None, page.body());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **No document may print an address other than [`CONTACT_EMAIL`].** Written against the
    /// personal address these pages used to carry: a support address that reaches only some of the
    /// screens is worse than none, because the reader cannot tell which one is current. The scan
    /// is for `@` rather than for the old address, so the NEXT stray address fails too.
    #[test]
    fn every_document_prints_only_the_one_contact_address() {
        for page in Page::ALL {
            let local_len = CONTACT_EMAIL.find('@').expect("CONTACT_EMAIL has a local part");
            for (i, _) in page.body().match_indices('@') {
                // `checked_sub`, not `i - local_len`: an `@` closer to the start of a body than the
                // address's own local part is exactly the stray this test hunts, and subtracting
                // past zero on a usize would panic with an arithmetic message instead of the
                // finding.
                let tail = i
                    .checked_sub(local_len)
                    .map(|start| &page.body()[start..])
                    .unwrap_or("");
                assert!(
                    tail.starts_with(CONTACT_EMAIL),
                    "{:?} prints an address that is not CONTACT_EMAIL",
                    page.title()
                );
            }
        }
    }

    /// **The policy must describe the build it ships in.** These assertions pin CLAIMS, not
    /// wording: each names a fact about this application that the document was silently wrong or
    /// silent about, and each was RED when it was written.
    ///
    /// * Nothing in the persisted session carries a playhead: `session.rs` has no such field, and
    ///   position is read from and reported to the server. **Do not cite `coldstart.rs` as the
    ///   evidence** — what it retired is `lastplace.json`, the last-PAGE/route bookmark (which
    ///   Detail or Library screen a cold boot reopened), which is a different artefact that was
    ///   conflated with a playhead when this test was written.
    /// * `session.rs` persists `recent_searches` — the exact search terms, per Home profile — plus
    ///   per-server tokens and server addresses. None were listed.
    /// * There are TWO identifiers and they never travel together: `install_id` goes to PostHog
    ///   only (`telemetry/sender.rs`) and `errors_id` to Sentry only, as `user.id`
    ///   (`telemetry/sentry.rs::attach_user`). The policy used to promise that a crash report
    ///   "carries no installation identifier" and "cannot be linked to any other report"; both
    ///   became false the day the crash-report id was added, and the assertion below is what
    ///   would have caught a policy that still said so.
    /// * The sign-in prefers external candidates, but `paths.rs` now has the packaged app-local
    ///   `state/auth.json` fallback; likewise the reporting decision can use `state/telemetry.json`.
    ///   External files may survive an uninstall, while app-local state is removed with its app.
    ///
    /// Reword the document freely; when you do, move the assertion with it deliberately rather
    /// than deleting it.
    #[test]
    fn the_policy_describes_what_this_build_actually_does() {
        let p = Page::Privacy.body();
        assert!(
            !p.contains("Home library choices, playback position"),
            "nothing in the session schema stores a playhead; the server holds position"
        );
        for claim in [
            "recent searches",
            "Analytics ID",
            "Crash report ID",
            "RETENTION",
            "WHERE DATA IS PROCESSED",
            "UNINSTALLING",
        ] {
            assert!(p.contains(claim), "the policy never mentions {claim:?}");
        }
        for stale in [
            "carries no installation identifier",
            "cannot be linked",
            "cannot be found or deleted",
            // consent and both identifiers END with the sign-in since 2026-09-04
            "NOT removed by signing out",
        ] {
            assert!(
                !p.contains(stale),
                "the policy still claims crash reports are anonymous: {stale:?}"
            );
        }
        // The two identifier names the Settings rows use are the names the policy uses.
        assert!(p.contains("Settings shows it as your Crash report ID"));
        assert!(p.contains("Settings shows that identifier as your Analytics ID"));
        // …and the policy says what `auth::forget_account` does to them.
        assert!(p.contains("signing out removes them with it"));
    }
    #[test]
    fn legal_has_six_current_documents() {
        assert_eq!(Page::ALL.len(), 6);
    }
    /// The About page names the binary the user is RUNNING.
    ///
    /// It was a hand-typed `PlxNative 0.5.0` that no bump script touched, so it could only ever
    /// have been right by accident. Written against `identity::VERSION` rather than against
    /// `env!` again so that re-typing a literal here fails: on any developer build the two differ.
    #[test]
    fn about_names_the_running_version() {
        let v = crate::plex::identity::VERSION;
        assert!(
            ABOUT.contains(&format!("Version {v}")),
            "About should name {v}, says: {ABOUT:?}"
        );
    }

    /// The About page also names the exact COMMIT — `PLX_VERSION` alone cannot distinguish two
    /// trunk builds cut minutes apart, since both report the same `X.Y.0-dev`.
    #[test]
    fn about_names_a_build_sha() {
        assert!(
            ABOUT.contains("\nBuild "),
            "About should carry a Build line, says: {ABOUT:?}"
        );
        assert!(
            !env!("PLX_BUILD_SHA").is_empty(),
            "PLX_BUILD_SHA must never be the empty string (build.rs falls back to \"unknown\")"
        );
    }

    /// **RIGHT enters, LEFT leaves — on the index as well as inside a document.** `ui::legal` was
    /// the one screen in the family that already had half of this rule, and the half it was
    /// missing is the one issue 6 names: LEFT on the INDEX did nothing at all, so the family's
    /// "navigate back with Left" only worked one level deep.
    #[test]
    fn right_enters_a_document_and_left_walks_all_the_way_back_out() {
        let _g = crate::testlock::serial();
        open();
        assert!(is_open() && !detail());
        assert!(on_left_right(1), "RIGHT on a chevron row opens it");
        assert!(detail(), "…and the document is up");
        assert!(on_left_right(-1), "LEFT inside a document returns to the index");
        assert!(!detail());
        assert!(
            on_left_right(-1),
            "…and LEFT on the index, which has no action band, is the way back to Settings"
        );
        assert!(
            !is_open(),
            "a screen with nothing to its left leaves on LEFT (rule 9)"
        );
        close();
    }

    /// **Rule 11.** The Legal index's rows are hoverable and clickable; a pushed document has no
    /// target, so a click over one parks nothing rather than closing it by accident. `app.rs` used
    /// to swallow every click over this whole route.
    #[test]
    fn hover_parks_an_index_row_and_a_document_has_no_click_target() {
        let _g = crate::testlock::serial();
        open();
        // Rule 11 refuses a positional hit until the layer has arrived — land the entrance first.
        for _ in 0..600 {
            update(1.0 / 60.0);
        }
        let f = RouteLayout::screen().sectioned_table();
        // Scan the column rather than guessing a row's y — the table owns its header band and
        // paddings, and a literal here would be a transcription of them.
        let mut parked = std::collections::BTreeSet::new();
        let mut a_row_y = None;
        for i in 0..80 {
            let y = f.y + 10.0 + i as f32 * 12.0;
            if pointer_focus(f.x + 40.0, y) {
                parked.insert(table().sel);
                a_row_y.get_or_insert(y);
            }
        }
        assert!(
            parked.len() >= 2,
            "hover must park different rows at different heights, parked {parked:?}"
        );
        assert!(
            !pointer_focus(f.x + 40.0, f.y - 400.0),
            "above the list is dead space"
        );
        let y = a_row_y.expect("some y in the content column is over a row");
        assert!(click(f.x + 40.0, y), "a row opens on the click");
        assert!(detail(), "…and the document is up");
        for _ in 0..600 {
            update(1.0 / 60.0);
        }
        assert!(
            !pointer_focus(f.x + 40.0, y),
            "a document has no row to park on"
        );
        close();
    }

    #[test]
    fn plex_boundary_is_explicit() {
        assert!(PRIVACY.contains(
            "Plex processes information received by those services under Plex’s own Privacy Policy"
        ));
        assert!(PRIVACY.contains("https://www.plex.tv/about/privacy-legal/"));
        assert!(!TRADEMARKS.contains("used under licence"));
    }

    /// **Issue #75, mirrored from `PRIVACY.md`**: the full policy names both sign-in reporting
    /// paths — the automatic handled-error report the crash switch gates, and the one-off report
    /// the sign-in screen can offer regardless of that switch, which must say plainly it carries
    /// no identifier.
    #[test]
    fn privacy_names_both_signin_reporting_paths() {
        assert!(PRIVACY.contains("handled sign-in error report"));
        assert!(PRIVACY.contains("one-off report about a specific sign-in problem"));
        assert!(PRIVACY.contains("no identifier that persists between reports"));
    }

    #[test]
    fn privacy_explains_oneoff_receipts_and_unwritable_queue_delivery() {
        assert!(PRIVACY.contains("one-off Report ID"));
        assert!(PRIVACY.contains("directly from memory"));
        assert!(PRIVACY.contains("startup read from a later fresh sign-in save"));
        assert!(!PRIVACY.contains("no handle at all"));
        assert!(!PRIVACY.contains("cannot be looked up or deleted on request"));
        assert!(PRIVACY.contains("no identifier that persists between reports"));
    }

    /// **Issue #76, mirrored from `PRIVACY.md`**: the storage error report and the
    /// `session_storage` usage fact are both named, and neither ever carries key material.
    #[test]
    fn privacy_names_the_storage_error_report_and_the_usage_fact() {
        assert!(PRIVACY.contains("handled storage error report"));
        assert!(PRIVACY.contains("how the saved sign-in is stored on this television"));
        assert!(PRIVACY.contains("no key material"));
    }

    /// **Issue #76, second half, mirrored from `PRIVACY.md`**: a storage-error report found before
    /// the crash-reports question is answered waits in memory rather than being sent or silently
    /// dropped, and the notice must say so plainly — otherwise it still reads like every report is
    /// either sent now or lost for good, which stopped being true once `telemetry::storage` started
    /// deferring one instead of dropping it.
    #[test]
    fn privacy_states_the_storage_report_defers_before_the_question_is_answered() {
        assert!(PRIVACY.contains("It is sent only once you have answered the crash-reports question Yes"));
        assert!(PRIVACY.contains("waits in memory"));
        assert!(PRIVACY.contains("discarded if you answer No or if the app closes before you answer"));
    }

    /// **Consent scope versions (owner decision, 2026-09-10), mirrored from `PRIVACY.md`**: a
    /// declined extension must be stated as distinct from switching a category off — otherwise the
    /// policy text still reads like the old "a No stays a No" withdrawal model that `apply_extension`
    /// no longer implements.
    #[test]
    fn privacy_distinguishes_a_declined_extension_from_switching_a_category_off() {
        assert!(PRIVACY.contains("Declining an expansion is not the same as switching a category off"));
        assert!(PRIVACY.contains("sends nothing of the new part"));
        assert!(PRIVACY.contains("stays a separate action"));
    }

    /// **Stage B2 (issue #76), mirrored from `PRIVACY.md`**: the cross-launch storage probe is
    /// described in plain language, and its two on-disk markers are named — otherwise "how your
    /// sign-in is stored" stays a claim about the OLD single-launch model even though a fresh
    /// sign-in on an unproven install no longer seals anything at all until a later launch proves
    /// the key service can be trusted.
    #[test]
    fn privacy_describes_the_cross_launch_storage_probe_and_its_two_markers() {
        assert!(PRIVACY.contains("checks, on a later launch, that the television's key service can still open something it sealed before"));
        assert!(PRIVACY.contains("secure-storage.proven"));
        assert!(PRIVACY.contains("secure-storage.refused"));
    }

    /// **Stage B2 (issue #76), mirrored from `PRIVACY.md`**: the storage error report now also
    /// covers a write that failed outright (every candidate path refused it), not only a seal/open
    /// call a key service answered. `legal.rs`'s condensed prose names the SHAPE rather than
    /// enumerating every closed code (it never has — `PRIVACY.md`'s own enumeration is what
    /// `telemetry::storage::tests::privacy_names_the_closed_storage_vocabulary` pins).
    #[test]
    fn privacy_names_a_write_that_fails_outright_in_the_storage_report() {
        assert!(PRIVACY.contains("or the file itself cannot be written at all"));
    }

    /// **Issue #76's identity decider, mirrored from `PRIVACY.md`**: the storage report names the
    /// key-outcome fact (existed vs newly created) and the owner_hint fact (whether the seal's own
    /// registration identified itself by an application id), both closed and both stated as
    /// possibly unknown rather than implied. The owner_hint half stopped being "always No" on
    /// 2026-09-10, when that registration started asking for the app id where nothing else in the
    /// process holds it (`keymanager`'s "Identity" section) — so the prose says what decides it.
    #[test]
    fn privacy_names_the_key_outcome_and_owner_hint_facts_in_the_storage_report() {
        assert!(PRIVACY.contains("already `existed` or was newly `created`")
            || PRIVACY.contains("already existed or was newly created"));
        assert!(PRIVACY.contains("application identity") || PRIVACY.contains("application id"));
    }

    /// **The two fields the notice gained on 2026-09-10 are the two this asserts** (review
    /// finding, 2026-09-11): they were added to the report under the "a new field inside the
    /// purpose you already read about" rule (`telemetry::consent`'s own doc), which is precisely
    /// the route that does NOT re-ask — so nothing but a test makes the notice actually describe
    /// them. `sealed_identity`'s closed vocabulary is asserted off `Identity::ALL` rather than a
    /// literal, for the reason `telemetry::storage`'s own vocabulary tests are: a hand-written
    /// list here had already gone stale once, in the commit that added `named`.
    #[test]
    fn privacy_describes_the_registration_name_and_the_sealed_identity() {
        // `registered_with_name` — the plain bus-NAME registration, as against the application
        // identity the sibling bool reports. The notice says it in words, not field names.
        assert!(PRIVACY.contains("a fixed name on the system bus"));
        // `sealed_identity` — whose key protects the file this report is about, which is a
        // different question from what THIS launch registered as, and the whole point of the field.
        assert!(PRIVACY.contains("which identity protected the saved sign-in"));
        for identity in crate::keymanager::Identity::ALL {
            assert!(
                PRIVACY.contains(identity.code()),
                "the notice never names the `{}` identity",
                identity.code()
            );
        }
        assert!(
            PRIVACY.contains("none where the report is about nothing protected at all"),
            "…and the absence case has to be a word too, since the report sends one"
        );
    }

    /// **Issue #76's quarantine, mirrored from `PRIVACY.md` and `SECURITY.md`**: a session file
    /// another app could have rewritten is not silently destroyed, and the owner cannot look for
    /// what was set aside unless the notice names it. Reset attempts that cleanup and must not
    /// claim it completed when the receipt says otherwise.
    #[test]
    fn privacy_names_the_quarantined_session_file() {
        assert!(PRIVACY.contains("<id>-auth.json.untrusted"));
        assert!(PRIVACY.contains(
            "signing out or Delete all local data attempts to remove it and reports if cleanup is incomplete"
        ));
        // The app-local fallback is packaged as an empty directory; runtime state is not part of
        // the payload and is removed with that application directory on uninstall.
        assert!(PRIVACY.contains("`state/auth.json`"));
        assert!(PRIVACY.contains("`state/telemetry.json`"));
        assert!(PRIVACY.contains("packaged app directory"));
    }

    /// **Issue #76's report lane (2026-09-11), mirrored from `PRIVACY.md`**: a fresh sign-in whose
    /// file could not be kept is named as its own case of the storage error report, the one-off
    /// report says it may carry the same facts, and the candidate-location summary is described as
    /// closed words and numbers rather than a path — `legal.rs`'s condensed prose names the SHAPE
    /// rather than enumerating every closed code, the same reasoning
    /// [`privacy_names_a_write_that_fails_outright_in_the_storage_report`] gives for `write_failed`.
    #[test]
    fn privacy_names_a_sign_in_that_could_not_be_persisted() {
        assert!(PRIVACY.contains("a fresh sign-in whose file could not be kept"));
        assert!(PRIVACY.contains("what the save actually did"));
        assert!(PRIVACY.contains("never a file path"));
        assert!(PRIVACY.contains("may carry the same save-outcome and candidate-location facts"));
    }

    /// **The candidate summary's outcome words are a CLOSED list, and the on-screen notice has to
    /// say so** — including the one added on 2026-09-11 for a candidate whose bytes are none of
    /// the shapes this build knows (`plex::session::ReadRejection::Unparsable`, wire
    /// `unparsable`). That word describes a file whose CONTENT was rejected, which is exactly the
    /// class a reader of this notice would otherwise assume is never examined, so the notice says
    /// what is sent about it and what is not.
    #[test]
    fn privacy_says_the_candidate_summary_covers_an_unrecognised_file() {
        assert!(PRIVACY.contains("each outcome one closed word"));
        assert!(PRIVACY.contains("contents were not recognised at all"));
    }
}
