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

const PRIVACY: &str = "RESPONSIBLE FOR PLXNATIVE DATA\n\nGleb Linnik is responsible only for data PlxNative stores locally and for optional reports you choose to share.\n\nPLEX SERVICES\n\nPlxNative is an independent client for Plex. To sign you in, discover servers and provide Plex account features, the app communicates directly with Plex services. Plex processes information received by those services under Plex’s own Privacy Policy. PlxNative’s developer does not receive that information.\n\nPlex Privacy Policy: https://www.plex.tv/about/privacy-legal/\n\nPLEX MEDIA SERVERS\n\nTo browse and play media, update watch progress and use server features, PlxNative communicates directly with the Plex Media Servers you select. Those requests are handled by the selected server and its operator. PlxNative’s developer does not receive them.\n\nON THIS TELEVISION\n\nPlxNative stores your Plex account token and a separate token for each server you use, the addresses and identifiers of those servers, the profile you selected together with the profile names and pictures on your account, your Home library choices, your recent searches, your playback quality preference, and a small rotating local log. It also stores your answers to the two optional-reporting questions, the random Crash report ID if you turned crash reports on, the random Analytics ID if you turned product analytics on, any report waiting to be sent, and a marker recording how much of the crash log has already been read. It keeps no bookmark of its own for where you stopped watching: playback position is held by your Plex Media Server. Delete all local data in Settings signs out and removes PlxNative data from this television.\n\nOPTIONAL CRASH REPORTS\n\nIf enabled, technical crash details are sent to Sentry in Germany. They can include the signal, code addresses, thread information and device compatibility details. Each report carries a random Crash report ID created on this television when you turned crash reports on, so that repeated crashes under one Crash report ID are counted once rather than once each. It is not derived from your Plex account, your television or anything about you, and it is never sent with product analytics. Settings shows it as your Crash report ID while crash reports are on.\n\nOPTIONAL PRODUCT ANALYTICS\n\nIf enabled, screen and feature events and broad sign-in and playback outcomes are sent to PostHog in Germany with a random Analytics ID created when you turned product analytics on. Settings shows that identifier as your Analytics ID while product analytics is on.\n\nNEVER INCLUDED\n\nTitles, Plex accounts, searches, server names or addresses, tokens, subtitle text and exact viewing history are not included in either optional report type. Both choices are independent and can be changed at any time in Settings.\n\nRETENTION\n\nDifferent things here have different lifetimes, so this is stated for each. Your sign-in, the servers registered with it and their tokens are removed when you sign out. Your answers to the two optional-reporting questions, and the Crash report ID and Analytics ID if they exist, belong to that sign-in: signing out removes them with it, and whoever signs in next is asked afresh. Switching between the profiles of one Plex account is not a sign-out and keeps them. A report waiting to be sent is deleted once it is sent, and a queued report of a category you switch off, or that you sign out of, is deleted at that moment; one report that the sender had already picked up at that moment may still be sent, and no further report is picked up after it. The local log rotates, so its oldest lines are discarded continuously. Delete all local data removes all of it. A report that has already been sent is held by the service that received it, under that service’s own retention schedule; write to the contact below to ask what those periods currently are.\n\nYOUR CHOICES AND HOW TO ASK\n\nBoth optional reports are off until you turn them on, and either can be turned off again at any time in Settings. Delete all local data removes what PlxNative stored on this television; it does not reach anything already sent. To ask what crash reports or product analytics hold for your installation, or to have them deleted, write to the contact below and quote your Crash report ID or Analytics ID from Settings. Each identifier is the only handle its reports carry. Turning a category off deletes its identifier from this television, and so does signing out; reports already sent keep the old one, so copy it down first if you intend to ask for their deletion.\n\nWHERE DATA IS PROCESSED\n\nOptional crash reports are processed by Sentry in Germany and optional product analytics by PostHog in Germany. Plex processes what its own services receive under Plex’s Privacy Policy. A Plex Media Server you connect to may be located anywhere and is operated by whoever runs it, not by PlxNative’s developer.\n\nUNINSTALLING\n\nRemoving PlxNative removes the application, but webOS gives an application no way to run code as it is removed, so anything kept outside the application’s own directory can survive. Two things are deliberately kept there: your sign-in, so that reinstalling does not sign you out, and — because they belong to that sign-in — your optional-reporting answers together with the Crash report ID and Analytics ID, so that a decision you have already made is not put to you again after a reinstall. Use Delete all local data BEFORE uninstalling if you want nothing of PlxNative left on this television.\n\nCONTACT\n\nPrivacy questions: support@plxnative.com";
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
    /// * The sign-in is stored OUTSIDE the app directory on purpose (`paths.rs`), so it survives a
    ///   reinstall. "Uninstall removes everything" would have been false.
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
}
