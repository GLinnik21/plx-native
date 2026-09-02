//! Legal and About routes on the Settings modal's frozen full-screen ground.

use crate::ui::consts::SCR_W;
use crate::ui::document_reader::DocumentReader;
use crate::ui::popover::Popover;
use crate::ui::route_screen::{RouteLayout, RoutePush};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::theme;
use std::ptr::{addr_of, addr_of_mut};

/// The index's own title, and — one push deeper — the crumb a document names.  One constant, so
/// the two can never disagree about what the screen behind you is called.
const INDEX_TITLE: &str = "Legal notices";
/// Where BACK goes from this family's two entrances. Both are opened from the Settings root.
const CRUMB_SETTINGS: &str = "Settings";

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
    fn body(self) -> &'static str {
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

const PRIVACY: &str = "RESPONSIBLE FOR PLXNATIVE DATA\n\nGleb Linnik is responsible only for data PlxNative stores locally and for optional reports you choose to share.\n\nPLEX SERVICES\n\nPlxNative is an independent client for Plex. To sign you in, discover servers and provide Plex account features, the app communicates directly with Plex services. Plex processes information received by those services under Plex’s own Privacy Policy. PlxNative’s developer does not receive that information.\n\nPlex Privacy Policy: https://www.plex.tv/about/privacy-legal/\n\nPLEX MEDIA SERVERS\n\nTo browse and play media, update watch progress and use server features, PlxNative communicates directly with the Plex Media Servers you select. Those requests are handled by the selected server and its operator. PlxNative’s developer does not receive them.\n\nON THIS TELEVISION\n\nPlxNative stores the session needed to sign in, the selected profile, Home library choices, playback position, settings and a small rotating local log. Delete all local data in Settings signs out and removes PlxNative data from this television.\n\nOPTIONAL CRASH REPORTS\n\nIf enabled, technical crash details are sent to Sentry in Germany. They can include the signal, code addresses, thread information and device compatibility details.\n\nOPTIONAL PRODUCT ANALYTICS\n\nIf enabled, screen and feature events and broad sign-in and playback outcomes are sent to PostHog in Germany with a random installation identifier.\n\nNEVER INCLUDED\n\nTitles, Plex accounts, searches, server names or addresses, tokens, subtitle text and exact viewing history are not included in either optional report type. Both choices are independent and can be changed at any time in Settings.\n\nCONTACT\n\nPrivacy questions: glinnik21@gmail.com";
const OPEN_SOURCE: &str = "PlxNative is free software under the MIT Licence. Copyright (c) 2026 Gleb Linnik.\n\nThe application package includes THIRD-PARTY-NOTICES.md, the complete licence texts and font notices. Included projects include libcurl, SDL2, SDL2_ttf, nanosvg, zlib, jsmpeg, Inter, Noto Sans CJK, Feather, Heroicons, Material Icons and the Rust crates used by this build.";
const FFMPEG: &str = "This software uses libraries from the FFmpeg project under the LGPLv2.1. FFmpeg is copyright (c) the FFmpeg developers; PlxNative does not own FFmpeg.\n\nThe FFmpeg libraries are unmodified and loaded dynamically, and may be replaced with an interface-compatible build. The complete corresponding FFmpeg 9.0 source, exact configure line and build script are published with every PlxNative release.";
const SOURCE: &str = "PlxNative source code, release materials and build scripts are published at:\n\ngithub.com/GLinnik21/plx-native\n\nRelease source packages include the corresponding FFmpeg source and the script used to build it.";
const TRADEMARKS: &str = "Plex, the Plex logo and Plex Media Server are trademarks of Plex, Inc.\n\nLG and webOS are trademarks of LG Electronics Inc.\n\nPlxNative is an independent, unofficial application. It is not produced by, endorsed by, or affiliated with Plex, Inc. or LG Electronics Inc.";
const CONTACT: &str = "Privacy questions may be sent to glinnik21@gmail.com.\n\nSecurity vulnerabilities may be reported privately through GitHub Security Advisories for GLinnik21/plx-native. Please do not include Plex tokens, server addresses or personal media information in a report.";
/// The one page here that carries a NUMBER, and so the one that can go stale on its own.
///
/// It was a literal — `PlxNative 0.5.0`, hand-typed, and not on `ci/bump-version.py`'s list of
/// files to bump, so it was already the only surface in the app that could disagree with every
/// other. It is composed from the same `PLX_VERSION` the diagnostics panel and `X-Plex-Version`
/// report, so a release build says `0.5.0` here and a developer build says `0.5.1-dev`: this is
/// a screen a user is asked to read out in a bug report, and it must name the binary they are
/// running rather than the last thing that was published.
///
/// `concat!` rather than a `format!` at draw time: the whole page is a `&'static str` the reader
/// borrows, and `env!` is a literal at expansion.
const ABOUT: &str = concat!("VERSION\n\nPlxNative ", env!("PLX_VERSION"), "\n\nDEVELOPER\n\nGleb Linnik\n\nLICENCE\n\nMIT Licence\n\nPROJECT\n\ngithub.com/GLinnik21/plx-native\n\nPlxNative is an independent, unofficial application. It is not produced by, endorsed by, or affiliated with Plex, Inc. or LG Electronics Inc.");

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
    pop().close();
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
pub(crate) fn on_updown(delta: i32) -> bool {
    if !is_open() {
        return false;
    }
    if detail() {
        reader().move_by(delta);
    } else {
        table().move_sel(delta);
    }
    crate::ui::idle::invalidate();
    true
}
pub(crate) fn on_left_right(delta: i32) -> bool {
    if !is_open() {
        return false;
    }
    if delta > 0 {
        on_ok()
    } else if detail() {
        on_back()
    } else {
        true
    }
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
    if is_open() {
        pop().scrim(theme::alert::SCRIM_A);
    }
}
pub(crate) fn draw() {
    if !is_open() {
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
            "Version, copyright, project source and independent-client information.",
            theme::size::LABEL,
        );
        reader().draw(p, layout.content, None, ABOUT);
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
        reader().draw(document, layout.content, None, page.body());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            ABOUT.contains(&format!("PlxNative {v}")),
            "About should name {v}, says: {ABOUT:?}"
        );
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
