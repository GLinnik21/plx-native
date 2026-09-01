//! Legal and About routes on the Settings modal's frozen full-screen ground.

use crate::ui::consts::{MARGIN_X, SCR_H, SCR_W};
use crate::ui::popover::Popover;
use crate::ui::table::{Row, Section, TableView};
use crate::ui::text_view::TextView;
use crate::ui::{theme, Rect, Spring};
use std::ptr::{addr_of, addr_of_mut};

const TOP: f32 = 150.0;
const COPY_W: f32 = crate::ui::home::HERO_COL_W;
const LIST_ROOT_X: f32 = 930.0;
const LIST_DETAIL_X: f32 = 96.0;
const LIST_DETAIL_W: f32 = 660.0;
const DETAIL_X: f32 = 930.0;
const LEAD: f32 = 42.0;
const STEP: f32 = 210.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Page { Privacy, OpenSource, Ffmpeg, Source, Trademarks, Contact }
impl Page {
    pub(crate) const ALL: [Self; 6] = [Self::Privacy, Self::OpenSource, Self::Ffmpeg, Self::Source, Self::Trademarks, Self::Contact];
    fn title(self) -> &'static str { match self {
        Self::Privacy => "Privacy policy", Self::OpenSource => "Open-source licences",
        Self::Ffmpeg => "FFmpeg & source offer", Self::Source => "PlxNative source code",
        Self::Trademarks => "Trademarks & non-affiliation", Self::Contact => "Privacy & security contact",
    }}
    fn subtitle(self) -> &'static str { match self {
        Self::Privacy => "How PlxNative handles local data and optional reports.",
        Self::OpenSource => "Components, copyright holders and licence texts.",
        Self::Ffmpeg => "LGPL notice, replaceability and corresponding source.",
        Self::Source => "Project source, build scripts and release materials.",
        Self::Trademarks => "Independent-client status and trademark attribution.",
        Self::Contact => "How to ask a privacy question or report a vulnerability.",
    }}
    fn body(self) -> &'static str { match self {
        Self::Privacy => PRIVACY, Self::OpenSource => OPEN_SOURCE, Self::Ffmpeg => FFMPEG,
        Self::Source => SOURCE, Self::Trademarks => TRADEMARKS, Self::Contact => CONTACT,
    }}
}

const PRIVACY: &str = "RESPONSIBLE FOR PLXNATIVE DATA\n\nGleb Linnik is responsible only for data PlxNative stores locally and for optional reports you choose to share.\n\nPLEX SERVICES\n\nPlxNative is an independent client for Plex. To sign you in, discover servers and provide Plex account features, the app communicates directly with Plex services. Plex processes information received by those services under Plex’s own Privacy Policy. PlxNative’s developer does not receive that information.\n\nPlex Privacy Policy: https://www.plex.tv/about/privacy-legal/\n\nPLEX MEDIA SERVERS\n\nTo browse and play media, update watch progress and use server features, PlxNative communicates directly with the Plex Media Servers you select. Those requests are handled by the selected server and its operator. PlxNative’s developer does not receive them.\n\nON THIS TELEVISION\n\nPlxNative stores the session needed to sign in, the selected profile, Home library choices, playback position, settings and a small rotating local log. Delete all local data in Settings signs out and removes PlxNative data from this television.\n\nOPTIONAL CRASH REPORTS\n\nIf enabled, technical crash details are sent to Sentry in Germany. They can include the signal, code addresses, thread information and device compatibility details.\n\nOPTIONAL PRODUCT ANALYTICS\n\nIf enabled, screen and feature events and broad sign-in and playback outcomes are sent to PostHog in Germany with a random installation identifier.\n\nNEVER INCLUDED\n\nTitles, Plex accounts, searches, server names or addresses, tokens, subtitle text and exact viewing history are not included in either optional report type. Both choices are independent and can be changed at any time in Settings.\n\nCONTACT\n\nPrivacy questions: glinnik21@gmail.com";
const OPEN_SOURCE: &str = "PlxNative is free software under the MIT Licence. Copyright (c) 2026 Gleb Linnik.\n\nThe application package includes THIRD-PARTY-NOTICES.md, the complete licence texts and font notices. Included projects include libcurl, SDL2, SDL2_ttf, nanosvg, zlib, jsmpeg, Inter, Noto Sans CJK, Feather, Heroicons, Material Icons and the Rust crates used by this build.";
const FFMPEG: &str = "This software uses libraries from the FFmpeg project under the LGPLv2.1. FFmpeg is copyright (c) the FFmpeg developers; PlxNative does not own FFmpeg.\n\nThe FFmpeg libraries are unmodified and loaded dynamically, and may be replaced with an interface-compatible build. The complete corresponding FFmpeg 9.0 source, exact configure line and build script are published with every PlxNative release.";
const SOURCE: &str = "PlxNative source code, release materials and build scripts are published at:\n\ngithub.com/GLinnik21/plx-native\n\nRelease source packages include the corresponding FFmpeg source and the script used to build it.";
const TRADEMARKS: &str = "Plex, the Plex logo and Plex Media Server are trademarks of Plex, Inc.\n\nLG and webOS are trademarks of LG Electronics Inc.\n\nPlxNative is an independent, unofficial application. It is not produced by, endorsed by, or affiliated with Plex, Inc. or LG Electronics Inc.";
const CONTACT: &str = "Privacy questions may be sent to glinnik21@gmail.com.\n\nSecurity vulnerabilities may be reported privately through GitHub Security Advisories for GLinnik21/plx-native. Please do not include Plex tokens, server addresses or personal media information in a report.";
const ABOUT: &str = "VERSION\n\nPlxNative 0.5.0\n\nDEVELOPER\n\nGleb Linnik\n\nLICENCE\n\nMIT Licence\n\nPROJECT\n\ngithub.com/GLinnik21/plx-native\n\nPlxNative is an independent, unofficial application. It is not produced by, endorsed by, or affiliated with Plex, Inc. or LG Electronics Inc.";

static mut POP: Popover = Popover::new();
static mut TABLE: TableView = TableView::new();
static mut PAGE: Page = Page::Privacy;
static mut DETAIL: bool = false;
static mut ABOUT_MODE: bool = false;
static mut MORPH: Spring = Spring::at(0.0);
static mut SCROLL: f32 = 0.0;
static mut MAX_SCROLL: f32 = 0.0;
fn pop() -> &'static mut Popover { unsafe { &mut *addr_of_mut!(POP) } }
fn table() -> &'static mut TableView { unsafe { &mut *addr_of_mut!(TABLE) } }
pub(crate) fn is_open() -> bool { unsafe { (*addr_of!(POP)).is_open() } }
fn detail() -> bool { unsafe { *addr_of!(DETAIL) } }

fn build() {
    let mut s = Section::new("Legal");
    for page in Page::ALL { s = s.row(Row::new(page.title()).detail(page.subtitle()).chevron(true)); }
    table().compact = false;
    table().set_sections(vec![s], 0, false);
}
pub(crate) fn open() {
    build();
    unsafe { PAGE = Page::Privacy; DETAIL = false; ABOUT_MODE = false; (*addr_of_mut!(MORPH)).jump(0.0); SCROLL = 0.0; }
    table().list_focused = true; pop().open(); crate::ui::idle::invalidate();
}
pub(crate) fn open_about() {
    unsafe { ABOUT_MODE = true; DETAIL = true; (*addr_of_mut!(MORPH)).jump(1.0); SCROLL = 0.0; }
    pop().open(); crate::ui::idle::invalidate();
}
pub(crate) fn close() { pop().close(); crate::ui::idle::invalidate(); }
pub(crate) fn on_back() -> bool {
    if !is_open() { return false; }
    if detail() && unsafe { !ABOUT_MODE } {
        unsafe { DETAIL = false; SCROLL = 0.0; }
        table().list_focused = true; crate::ui::idle::invalidate();
    } else { close(); }
    true
}
pub(crate) fn on_ok() -> bool {
    if !is_open() { return false; }
    if !detail() {
        let i = table().sel.clamp(0, Page::ALL.len() as i32 - 1) as usize;
        unsafe { PAGE = Page::ALL[i]; DETAIL = true; SCROLL = 0.0; }
        table().list_focused = false; crate::ui::idle::invalidate();
    }
    true
}
pub(crate) fn on_updown(delta: i32) -> bool {
    if !is_open() { return false; }
    if detail() {
        unsafe { SCROLL = (SCROLL + delta as f32 * STEP).clamp(0.0, MAX_SCROLL); }
    } else { table().move_sel(delta); }
    crate::ui::idle::invalidate(); true
}
pub(crate) fn on_left_right(delta: i32) -> bool {
    if !is_open() { return false; }
    if delta > 0 { on_ok() } else if detail() { on_back() } else { true }
}
pub(crate) fn update(dt: f32) {
    pop().update(dt);
    unsafe { (*addr_of_mut!(MORPH)).step(if DETAIL { 1.0 } else { 0.0 }, 200.0, dt); }
    table().update(dt, SCR_H as f32 - TOP - MARGIN_X);
}
pub(crate) fn draw_scrim() { if is_open() { pop().scrim(theme::alert::SCRIM_A); } }
pub(crate) fn draw() {
    if !is_open() { return; }
    let pop = pop();
    let a = pop.appear();
    let p = pop.content_painter(0.0).alpha(a).translate(SCR_W as f32 * (1.0 - a), 0.0);
    if unsafe { ABOUT_MODE } {
        draw_doc(p, "About PlxNative", ABOUT, Rect::new(DETAIL_X, TOP, SCR_W as f32 - MARGIN_X - DETAIL_X, SCR_H as f32 - TOP - MARGIN_X));
        return;
    }
    let t = unsafe { MORPH.pos.clamp(0.0, 1.0) };
    let lx = LIST_ROOT_X + (LIST_DETAIL_X - LIST_ROOT_X) * t;
    let lw = (SCR_W as f32 - MARGIN_X - LIST_ROOT_X) + (LIST_DETAIL_W - (SCR_W as f32 - MARGIN_X - LIST_ROOT_X)) * t;
    if t < 0.999 {
        let cp = p.alpha(1.0 - t);
        TextView::new("Legal notices", theme::size::HERO, theme::TEXT_HEADING).bold().draw(cp, Rect::new(MARGIN_X, TOP, COPY_W, 180.0));
        TextView::new("Read the notices that apply to this build, its open-source components and its relationship with Plex and LG.", theme::size::BODY, theme::TEXT_READING).max_lines(5).draw(cp, Rect::new(MARGIN_X, TOP + 126.0, COPY_W, 280.0));
    }
    table().draw(p, Rect::new(lx, TOP, lw, SCR_H as f32 - TOP - MARGIN_X));
    if t > 0.01 {
        let page = unsafe { PAGE };
        draw_doc(p.alpha(t), page.title(), page.body(), Rect::new(DETAIL_X, TOP, SCR_W as f32 - MARGIN_X - DETAIL_X, SCR_H as f32 - TOP - MARGIN_X));
    }
}

fn draw_doc(p: crate::ui::Painter, title: &str, body: &str, frame: Rect) {
    let title_view = TextView::new(title, theme::size::HEADLINE, theme::TEXT_HEADING).bold();
    let th = title_view.measure_h(frame.w); title_view.draw(p, frame);
    let body_frame = Rect::new(frame.x, frame.y + th + theme::space::MD, frame.w, frame.h - th - theme::space::MD);
    let view = TextView::new(body, theme::size::BODY, theme::TEXT_SECONDARY).leading(LEAD);
    let h = view.measure_h(body_frame.w);
    unsafe { MAX_SCROLL = (h - body_frame.h).max(0.0); SCROLL = SCROLL.min(MAX_SCROLL); }
    p.clip(body_frame);
    view.draw(p, Rect::new(body_frame.x, body_frame.y - unsafe { SCROLL }, body_frame.w, h));
    p.clip_clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn legal_has_six_current_documents() { assert_eq!(Page::ALL.len(), 6); }
    #[test] fn plex_boundary_is_explicit() {
        assert!(PRIVACY.contains("Plex processes information received by those services under Plex’s own Privacy Policy"));
        assert!(PRIVACY.contains("https://www.plex.tv/about/privacy-legal/"));
        assert!(!TRADEMARKS.contains("used under licence"));
    }
}
