//! **Legal** — the privacy notice, the open-source notices, the source offer and the trademarks,
//! reachable from the account menu with the D-pad alone.
//!
//! # Why this screen exists, which is not "for completeness"
//!
//! Three separate obligations land on the same surface, and only one of them is about licences:
//!
//! * **LG's Privacy Guideline requires it in the APP**, not on the store listing: *"Provide Privacy
//!   Policy within your app."* The Data Safety declaration a submission carries is published beside
//!   the listing, but the policy itself has to be readable on the television. That is the one item
//!   here that is a submission blocker.
//! * **Plex's ToS** obliges an "Interfacing Software" author to *"provide and include (or link to)
//!   a privacy notice"*, and their trademark guidelines require an attribution line once a Plex
//!   mark appears in the UI — which it does, on nearly every screen.
//! * **LGPL-2.1 §6**'s third sentence: *"If the work during execution displays copyright notices,
//!   you must include the copyright notice for the Library among them."* Dormant until an app
//!   prints its OWN copyright at runtime — which the first row below does — and binding from that
//!   moment. This is why the FFmpeg notice is not optional decoration.
//!
//! **No licence in this app's stack requires an in-app licence VIEWER**, and neither does LG. The
//! `.ipk` already carries `THIRD-PARTY-NOTICES.md`, `TRADEMARKS.md`, `licenses/*.txt` and `OFL.txt`
//! as payload (`ci/check-package.py` asserts it), which is what discharges the "supply a copy"
//! duties for someone who received the package. This screen exists because *"the files are in the
//! archive"* is not reachable from a sofa, and because of the three obligations above — not because
//! a licence demanded a screen.
//!
//! # Shape
//!
//! Two levels, both [`Popover`], per `[[ui-menu-idiom]]`: no full-screen sheets, ever.
//!
//! 1. a [`TableView`] of sections — the same widget the account menu and the track menus use;
//! 2. a **scrollable prose reader** for the one you pick.
//!
//! The reader is the only new thing, and it is a `TextView` drawn into a frame offset by a scroll
//! position and hard-clipped to the panel, exactly as `TableView::draw` clips its own overflow.
//! `TableView` itself could not do this job: its rows **elide**, they do not wrap, so a paragraph
//! in a row is a paragraph with its end cut off.
//!
//! # The text is a `const` in this file, and that is deliberate
//!
//! It could be read from the `.ipk`'s own notice files at runtime. It is not, for two reasons.
//! Reading them means the screen can FAIL — a missing file, a jail that will not open it — and the
//! one screen that must not fail is the one a store reviewer opens. And the wording here is not the
//! same wording: `THIRD-PARTY-NOTICES.md` is the complete legal text for someone with the archive
//! and a scrollbar, while this is what a person reads across a room. `ci/check-package.py` gates
//! that the two agree about which libraries ship.
use crate::ui::consts::{SCR_H, SCR_W};
use crate::ui::popover::Popover;
use crate::ui::table::{Row, Section, TableView};
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::KeyHint;
use crate::ui::Rect;
use std::ptr::{addr_of, addr_of_mut};

// ---- the frame -------------------------------------------------------------------------------

/// Shared with the About panel: wide enough that prose wraps at a comfortable measure, narrow
/// enough to leave the page visible either side.
const PANEL_W: f32 = 1120.0;
const PAD: f32 = theme::alert::PAD;
const CONTENT_W: f32 = PANEL_W - 2.0 * PAD;
const EDGE_CLEAR: f32 = 68.0;
const SCRIM_A: f32 = theme::alert::SCRIM_A;
/// Reading leading, matching the About panel's synopsis run — this is the only screen in the app
/// anyone reads several paragraphs of.
const LEAD: f32 = 42.0; // size::BODY × 1.5
/// One D-pad press of the reader. A third of the visible column, so a press always leaves context
/// on screen: paging by a full column loses the line you were on, which on a 10-foot display is
/// how you lose your place entirely.
const SCROLL_STEP_FRAC: f32 = 0.33;

// ---- the sections ----------------------------------------------------------------------------

/// Which document is on screen. The order here is the order of the rows, and
/// [`Page::ALL`] is the single source of that — an enum arm without a row, or a row without an
/// arm, cannot happen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Page {
    Privacy,
    OpenSource,
    Source,
    Trademarks,
}

impl Page {
    /// Row order. `account_menu`'s own comment records the bug this shape avoids: a row appended in
    /// one place and not the other is exactly the index drift that made a menu open the wrong thing.
    pub(crate) const ALL: [Page; 4] = [
        Page::Privacy,
        Page::OpenSource,
        Page::Source,
        Page::Trademarks,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::Privacy => "Privacy",
            Page::OpenSource => "Open source licences",
            Page::Source => "Source code",
            Page::Trademarks => "Trademarks",
        }
    }

    /// The one-line summary under each row. It has to be true on its own, because on a television
    /// a good many people will read only this line and never open the page.
    fn subtitle(self) -> &'static str {
        match self {
            Page::Privacy => {
                "What this app stores and what it sends. Nothing is sent to its developer."
            }
            Page::OpenSource => "The open source components this app uses, and their licences.",
            Page::Source => {
                "Where to get the complete source, including the FFmpeg it was built from."
            }
            Page::Trademarks => "Plex and LG trademark attribution.",
        }
    }

    fn body(self) -> &'static str {
        match self {
            Page::Privacy => PRIVACY,
            Page::OpenSource => OPEN_SOURCE,
            Page::Source => SOURCE,
            Page::Trademarks => TRADEMARKS,
        }
    }
}

/// The privacy notice, first layer.
///
/// **This is the summary; `PRIVACY.md` in the repository is the full text**, and the URL below is
/// the second layer GDPR Art. 13 is satisfied by (WP260 allows layering: the screen carries who,
/// what, that it is optional, and where — the exhaustive list lives at the link). Everything here
/// is a statement about the code, checkable against the file named beside it.
///
/// **Rewritten when the sender landed, and by a test rather than by anyone remembering.** This used
/// to open "There is no analytics, no telemetry, no crash upload, and no server of mine for them to
/// reach", which was true and became false the moment `telemetry::sender` gained a `net::post_ca`
/// call. [`the_notice_cannot_claim_silence_while_this_build_can_send`] failed on that commit
/// and is the reason these words changed in it. The opening sentence is deliberately still a strong
/// claim — nothing is sent unless a switch is on, and the switches start off — because that is the
/// claim the code actually supports, and the honest version of a weaker one is not a vaguer
/// sentence but a more precise one.
const PRIVACY: &str = "\
PlxNative sends nothing to its developer unless you switch it on, and it is off until you do. \
There is no account with me. If you have not turned either switch on, this app has sent me nothing \
and holds nothing about you.

WHAT LEAVES THIS TELEVISION

plex.tv, to sign in and to list the servers your account can reach. Your Plex Media Servers, to \
browse and to play. Plex's own privacy policy governs what Plex sees; I am a third-party client and \
I receive none of it.

And, only if you asked for it: crash reports, and anonymous screens, features, sign-in and playback \
usage events. Two \
separate switches, both off by default, both reversible at any time in Settings, Privacy. Turning \
usage reporting off deletes the random identifier this television used, so anything sent later cannot be \
joined to anything sent before.

WHAT IS IN A REPORT

For a native crash: the signal, instruction and caller frames for the crashed and other captured \
threads, ARM registers, thread ids and internal labels, module basenames and addresses, and \
app/webOS/kernel build facts needed to reproduce and symbolicate it. Usage reports contain fixed \
screen/feature/sign-in/playback outcome and format classes, a per-attempt playback id, occurrence \
time and a random per-process session id. Neither schema has a field for titles, searches, \
subtitles, accounts or servers. You can read every exact schema, with runtime values shown as \
placeholders, on the same screen the switches are on.

Reports go to Sentry and PostHog, both in the European Union, and are kept no longer than 13 \
 months.

WHAT IS NEVER SENT

Titles. What you searched for. Subtitle text. Your account, your profile names, your server names \
and addresses, and the numbers that identify items in your library.

WHAT IS STORED HERE

Your sign-in, as one access token per server, in a file only this app can read. Where you were when \
you last closed the app. Your answer to the two switches above, and — only while one is on — the \
messages still waiting to be sent. Three log files, which a reboot clears.

WHAT THE LOG MAY NOT CONTAIN

Every line is scrubbed before it is written. Tokens, passwords, server addresses and names, your \
profile names, Plex GUIDs and search queries are rewritten. Media titles, what you typed into \
search, and subtitle text are never written at all. What remains is item numbers, which is what a \
playback bug is diagnosed from.

WHAT IS NEVER READ

The television's device identifier. webOS offers one derived from the MAC address; this app does \
not ask for it.

The full notice:
github.com/GLinnik21/plx-native/blob/main/PRIVACY.md";

/// The open-source notice.
///
/// The FFmpeg paragraph is doing three jobs at once and each sentence is load-bearing: LGPL-2.1 §6's
/// prominent notice, its "you do not own the Library and here is who does" (FFmpeg compliance
/// checklist item 12), and the §6(b) claim stated in the open where it can be checked — unmodified,
/// dynamically loaded, replaceable. "FFmpeg" is spelled with two capital Fs and a lowercase "mpeg",
/// which is checklist item 15 and is exactly what an editor will "fix".
const OPEN_SOURCE: &str = "\
PlxNative is free software under the MIT Licence. Copyright (c) 2026 Gleb Linnik.

It uses open source software. The complete notices, with each component's copyright holders and the \
full text of every licence, ship inside the application package as THIRD-PARTY-NOTICES.md and the \
licenses folder beside it, and are published at github.com/GLinnik21/plx-native.

FFmpeg

This software uses libraries from the FFmpeg project under the LGPLv2.1. FFmpeg is copyright (c) \
the FFmpeg developers; PlxNative does not own FFmpeg, and its authors and source are at ffmpeg.org. \
The FFmpeg libraries shipped with this application are unmodified and are loaded dynamically, and \
you may replace them with your own interface-compatible build. The complete corresponding source and \
the exact configure line used are published with every release.

Also included

libcurl, under the curl licence.
SDL2 and SDL2_ttf, under the zlib licence.
nanosvg, under the zlib licence.
zlib, under the zlib licence.
jsmpeg, under the MIT licence.
Inter, under the SIL Open Font License 1.1.
Noto Sans CJK, under the SIL Open Font License 1.1.
Feather and Heroicons, under the MIT licence.
Material Icons, under the Apache License 2.0.
Rust crates serde, serde_json, libc and image, under the MIT licence.";

/// The source offer.
///
/// Every compliant project surveyed does it this way — publish the source, on the same server as
/// the binary — rather than by a written offer. LGPL-2.1 §6(c) is an *alternative* that references
/// §6(a)'s materials rather than a free extra promise, and it can enlarge what has to be supplied,
/// so it is deliberately not made here.
const SOURCE: &str = "\
PlxNative is open source. The complete corresponding source for this build, including the FFmpeg \
sources it was compiled from and the exact configure line used to build them, is published as part \
of every release at:

github.com/GLinnik21/plx-native/releases

The application itself is under the MIT Licence; the FFmpeg libraries it loads are under the \
LGPLv2.1 and are unmodified.";

/// Trademark attribution.
///
/// The Plex line becomes mandatory the moment a Plex mark appears in the UI, which it does. The
/// "independent application" line is NOT required by anyone — it is one sentence, it is true, and
/// it is the thing a confused user actually needs, since LG ships an official Plex app too.
const TRADEMARKS: &str = "\
Plex, the Plex logo and Plex Media Server are trademarks of Plex, Inc., used under licence.

LG and webOS are trademarks of LG Electronics Inc.

PlxNative is an independent application. It is not produced by, endorsed by, or affiliated with \
Plex, Inc. or LG Electronics Inc.";

/// One block of a document: a heading or a paragraph.
///
/// **`TextView` word-wraps a single run and collapses newlines**, so handing it a whole document
/// produces one unbroken wall — the section headings run inline into the sentence before them.
/// That was how this screen first shipped and it is exactly what a legal notice read from a sofa
/// must not be, so the reader splits on blank lines and stacks the blocks itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Block<'a> {
    Heading(&'a str),
    Para(&'a str),
}

/// A block is a HEADING when it is a single short line carrying no lowercase letters — the
/// `WHAT LEAVES THIS TELEVISION` convention the document constants are written in. Detected rather
/// than marked up because the alternative is a second syntax inside a `&'static str`, and this one
/// is unambiguous: no sentence of prose in these documents is caps-only.
fn blocks(doc: &str) -> Vec<Block<'_>> {
    doc.split("\n\n")
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(|b| {
            if !b.contains('\n') && b.len() <= HEADING_MAX && !b.chars().any(|c| c.is_lowercase()) {
                Block::Heading(b)
            } else {
                Block::Para(b)
            }
        })
        .collect()
}

/// Longest run still treated as a heading. Comfortably past the longest one in these documents
/// (`WHAT THE LOG MAY NOT CONTAIN`, 28) and far short of any sentence.
const HEADING_MAX: usize = 60;

// ---- state -----------------------------------------------------------------------------------

static mut MENU_POP: Popover = Popover::new();
static mut READER_POP: Popover = Popover::new();
static mut TABLE: TableView = TableView::new(); // main-thread only
static mut PAGE: Page = Page::Privacy;
/// Reader scroll, in pixels from the top of the prose. Not a spring: this is a document, and a
/// document that glides after the key is released overshoots the line you stopped on.
static mut SCROLL: f32 = 0.0;
/// The last measured overflow, so [`scroll_by`] can clamp without re-wrapping the text outside a
/// draw. Written by [`draw`], read by the key handler — main-thread only, like everything here.
static mut MAX_SCROLL: f32 = 0.0;

#[allow(static_mut_refs)]
fn menu_pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(MENU_POP) }
}
#[allow(static_mut_refs)]
fn reader_pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(READER_POP) }
}
#[allow(static_mut_refs)]
fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}

pub(crate) fn is_open() -> bool {
    menu_open() || reader_open()
}
pub(crate) fn menu_open() -> bool {
    unsafe { (*addr_of!(MENU_POP)).is_open() }
}
pub(crate) fn reader_open() -> bool {
    unsafe { (*addr_of!(READER_POP)).is_open() }
}

/// Open the menu. Reachable from the account menu, and **without being signed in**: someone who
/// cannot sign in has still received a copy of this software, and the notices are theirs too.
pub(crate) fn open() {
    let mut sec = Section::new("Legal");
    for p in Page::ALL {
        sec = sec.row(Row::new(p.title()).detail(p.subtitle()).chevron(true));
    }
    table().set_sections(vec![sec], 0, false);
    debug_assert_eq!(Page::ALL.len() as i32, table().n_rows());
    menu_pop().open();
    crate::ui::idle::invalidate();
}

pub(crate) fn close() {
    if reader_open() {
        reader_pop().close();
    }
    if menu_open() {
        menu_pop().close();
    }
    crate::ui::idle::invalidate();
}

/// BACK: the reader closes back to the menu, the menu closes to the page. Reported so the caller's
/// key ladder still decides whether the press was spent.
pub(crate) fn on_back() -> bool {
    if reader_open() {
        reader_pop().close();
        crate::ui::idle::invalidate();
        return true;
    }
    if menu_open() {
        menu_pop().close();
        crate::ui::idle::invalidate();
        return true;
    }
    false
}

/// OK: on the menu, open the selected document. In the reader there is nothing to commit, so it
/// closes — the same reasoning `about_panel::on_ok` records, that a modal answering the remote's
/// primary button with nothing is the one dead key on the screen.
pub(crate) fn on_ok() -> bool {
    if reader_open() {
        reader_pop().close();
        crate::ui::idle::invalidate();
        return true;
    }
    if menu_open() {
        let sel = table().sel.clamp(0, Page::ALL.len() as i32 - 1);
        unsafe {
            addr_of_mut!(PAGE).write(Page::ALL[sel as usize]);
            addr_of_mut!(SCROLL).write(0.0);
        }
        reader_pop().open();
        crate::ui::idle::invalidate();
        return true;
    }
    false
}

/// UP/DOWN: move the menu selection, or scroll the reader.
pub(crate) fn on_updown(delta: i32) -> bool {
    if reader_open() {
        let step = (SCR_H as f32 * 0.55) * SCROLL_STEP_FRAC;
        scroll_by(delta as f32 * step);
        return true;
    }
    if menu_open() {
        table().move_sel(delta);
        crate::ui::idle::invalidate();
        return true;
    }
    false
}

/// Clamped against the overflow the last [`draw`] measured. Repaints: this is a discrete change
/// with no spring behind it, so `ui::idle` cannot see it otherwise — the class of bug the present
/// gate's own note calls out (`Xfade` and `Spinner` both shipped frozen).
fn scroll_by(dy: f32) {
    unsafe {
        let max = *addr_of!(MAX_SCROLL);
        let next = (*addr_of!(SCROLL) + dy).clamp(0.0, max);
        if next != *addr_of!(SCROLL) {
            addr_of_mut!(SCROLL).write(next);
            crate::ui::idle::invalidate();
        }
    }
}

pub(crate) fn update(dt: f32) {
    menu_pop().update(dt);
    reader_pop().update(dt);
}

// ---- draw ------------------------------------------------------------------------------------

/// The modal dim, drawn by the host page — see `about_panel::draw_scrim` for why the scrim belongs
/// between the page and the panel rather than with it.
pub(crate) fn draw_scrim() {
    if reader_open() {
        reader_pop().scrim(SCRIM_A);
    } else if menu_open() {
        menu_pop().scrim(SCRIM_A);
    }
}

pub(crate) fn draw() {
    if reader_open() {
        draw_reader();
    } else if menu_open() {
        draw_menu();
    }
}

fn draw_menu() {
    let pop = menu_pop();
    let h = (table().measured_height() + 2.0 * PAD).min(SCR_H as f32 - 2.0 * EDGE_CLEAR);
    let r = Rect {
        x: (SCR_W as f32 - PANEL_W) * 0.5,
        y: (SCR_H as f32 - h) * 0.5,
        w: PANEL_W,
        h,
    };
    let p = pop.content_painter(pop.appear());
    pop.panel(p, r, theme::ALERT_PANEL_RAD);
    table().draw(
        p,
        Rect {
            x: r.x + PAD,
            y: r.y + PAD,
            w: CONTENT_W,
            h: h - 2.0 * PAD,
        },
    );
}

fn draw_reader() {
    let pop = reader_pop();
    let page = unsafe { *addr_of!(PAGE) };
    let h = SCR_H as f32 - 2.0 * EDGE_CLEAR;
    let r = Rect {
        x: (SCR_W as f32 - PANEL_W) * 0.5,
        y: EDGE_CLEAR,
        w: PANEL_W,
        h,
    };
    let p = pop.content_painter(pop.appear());
    pop.panel(p, r, theme::ALERT_PANEL_RAD);

    // The title stays PUT while the prose scrolls under it — a heading that scrolls away leaves a
    // wall of text with nothing saying what it is, which on a screen you cannot scroll back on a
    // whim is worse than the vertical space it costs.
    let title = TextView::new(page.title(), theme::size::HEADLINE, theme::TEXT_PRIMARY).bold();
    let title_h = title.measure_h(CONTENT_W);
    title.draw(
        p,
        Rect {
            x: r.x + PAD,
            y: r.y + PAD,
            w: CONTENT_W,
            h: title_h,
        },
    );

    let hint = KeyHint::new(c"Press", c"BACK", c"to return");
    let hint_h = KeyHint::height() + KeyHint::pad_below();
    let body_top = r.y + PAD + title_h + theme::space::MD;
    let body_h = (r.y + h - PAD - hint_h - theme::space::MD) - body_top;

    // Measure the stack once, then draw it — the same list twice would let the overflow the key
    // handler clamps against describe a different layout from the one on screen.
    let blocks = blocks(page.body());
    let mut views: Vec<(f32, f32, TextView)> = Vec::with_capacity(blocks.len());
    let mut y = 0.0f32;
    for (i, b) in blocks.iter().enumerate() {
        let (tv, gap) = match b {
            // A heading takes the app's caps-header role — CAPTION, primary ink, bold — which is
            // the same face `table.rs` gives a section header, so a document's structure reads the
            // way a menu's does.
            Block::Heading(t) => (
                TextView::new(t, theme::size::CAPTION, theme::TEXT_PRIMARY).bold(),
                theme::space::LG,
            ),
            Block::Para(t) => (
                TextView::new(t, theme::size::BODY, theme::TEXT_SECONDARY).leading(LEAD),
                theme::space::MD,
            ),
        };
        // The gap belongs to the block BELOW it, so the first block never pays one — the same
        // margin-top rule `about_panel`'s ladder is written to.
        if i > 0 {
            y += gap;
        }
        let h = tv.measure_h(CONTENT_W);
        views.push((y, h, tv));
        y += h;
    }
    let full_h = y;

    // Publish the overflow for the key handler. Written every frame the reader is up, so it can
    // never describe a page other than the one on screen.
    let max = (full_h - body_h).max(0.0);
    unsafe { addr_of_mut!(MAX_SCROLL).write(max) };
    let scroll = unsafe { (*addr_of!(SCROLL)).min(max) };

    // Hard-clip to the body column and draw the stack offset upward, the way `TableView::draw`
    // clips its own overflow. Released before returning — the clip is global GL state and
    // `ui::guard` documents what a leaked one does to every later frame.
    let clip = Rect {
        x: r.x + PAD,
        y: body_top,
        w: CONTENT_W,
        h: body_h,
    };
    p.clip(clip);
    for (by, bh, tv) in &views {
        let top = clip.y - scroll + by;
        // Cull whole blocks outside the column — the scissor would clip them anyway, but a long
        // document is a dozen wrapped runs and there is no reason to lay out the ones off screen.
        if top + bh < clip.y || top > clip.y + clip.h {
            continue;
        }
        tv.draw(
            p,
            Rect {
                x: clip.x,
                y: top,
                w: CONTENT_W,
                h: *bh,
            },
        );
    }
    p.clip_clear();

    // Right-pinned on the footer baseline, the form `about_panel` settled on for this family —
    // its own note says not to re-derive the centred variant.
    hint.draw(
        p,
        r.x + r.w - PAD - hint.width(),
        r.y + h - PAD - KeyHint::height() * 0.5,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Row order IS the index→page map, so the two cannot be allowed to drift. `account_menu`
    /// records the same hazard from the other side, where a row appended without an action arm
    /// opened the wrong screen.
    #[test]
    fn every_page_has_a_row_and_every_row_a_page() {
        assert_eq!(Page::ALL.len(), 4);
        for p in Page::ALL {
            assert!(!p.title().is_empty());
            assert!(!p.subtitle().is_empty());
            assert!(!p.body().is_empty());
        }
    }

    /// **The obligations, asserted as text.** Each of these is a sentence somebody is contractually
    /// owed, and each is the kind of thing an editor tidies away without knowing it was load-bearing.
    #[test]
    fn the_required_notices_are_present_and_correctly_worded() {
        // LGPL-2.1 §6 prominent notice + FFmpeg compliance checklist item 10
        assert!(OPEN_SOURCE.contains("libraries from the FFmpeg project under the LGPLv2.1"));
        // checklist item 12 — you do not own it, and here is who does
        assert!(OPEN_SOURCE.contains("does not own FFmpeg"));
        assert!(OPEN_SOURCE.contains("ffmpeg.org"));
        // the §6(b) claim, stated where it can be checked
        assert!(OPEN_SOURCE.contains("unmodified") && OPEN_SOURCE.contains("loaded dynamically"));
        // checklist item 15 — two capital Fs, lowercase "mpeg". An editor "fixes" this to FFMpeg.
        assert!(!OPEN_SOURCE.contains("FFMpeg") && !OPEN_SOURCE.contains("ffmpeg project"));
        // LGPL §4/§6(d) — where the corresponding source is
        assert!(SOURCE.contains("github.com/GLinnik21/plx-native"));
        // Plex trademark guidelines, binding because Plex marks appear in this UI
        assert!(TRADEMARKS.contains("trademarks of Plex, Inc."));
        // GDPR Art. 13 first layer: the pointer to the full notice
        assert!(PRIVACY.contains("PRIVACY.md"));
    }

    /// **The privacy page must not claim more than the code does**, in the direction that matters:
    /// these are the sentences a viewer relies on, so each is pinned to the wording the mechanism
    /// actually supports.
    ///
    /// It was called `the_privacy_page_states_the_no_telemetry_position_it_can_be_held_to` and
    /// asserted that the page claimed silence, which was right until a sender existed and is now
    /// backwards as a NAME while the body has been rewritten around it. A test whose name says the
    /// opposite of what it checks is worse than no test: it is the thing a reader greps for to
    /// confirm the guarantee, and it tells them the guarantee is the old one.
    #[test]
    fn the_privacy_page_states_the_consent_position_it_can_be_held_to() {
        // The claim as it now stands: nothing is sent unless a switch is on, and they start off.
        assert!(PRIVACY.contains("sends nothing to its developer unless you switch it on"));
        assert!(PRIVACY.contains("off until you do"));
        assert!(
            PRIVACY.contains("both off by default"),
            "the default is the point"
        );
        assert!(PRIVACY.contains("reversible at any time"), "Art. 7(3)");
        assert!(
            PRIVACY.contains("does not ask for it"),
            "the LGUDID position"
        );
    }

    /// The page names the two recipients, the region and the retention — the three facts a reader
    /// cannot check for themselves and which the LG Data Safety declaration also has to state.
    /// Asserted so the screen and that filing cannot drift apart silently.
    #[test]
    fn the_privacy_page_names_the_recipients_region_and_retention() {
        assert!(PRIVACY.contains("Sentry") && PRIVACY.contains("PostHog"));
        assert!(PRIVACY.contains("European Union"));
        assert!(PRIVACY.contains("13 months"));
    }

    /// And it repeats the payload claim the consent screen makes, in the same terms — a person who
    /// finds this page months later must be able to check the promise they were given.
    #[test]
    fn the_privacy_page_repeats_the_payload_claim() {
        assert!(
            PRIVACY.contains("Titles."),
            "the never-sent list leads with titles"
        );
        assert!(PRIVACY.contains("What you searched for."));
        assert!(PRIVACY.contains("Neither schema has a field for titles"));
        assert!(PRIVACY.contains("module basenames"));
    }

    /// The blank-line split is what turns a wall of text into a document, so it is pinned rather
    /// than left to the eye — the first version of this screen handed the whole string to one
    /// `TextView`, which collapses newlines, and every section heading ran inline into the sentence
    /// before it.
    #[test]
    fn a_document_splits_into_headings_and_paragraphs() {
        let bs = blocks(PRIVACY);
        assert!(
            bs.len() >= 8,
            "the privacy notice is a document, not one run: {}",
            bs.len()
        );
        assert!(
            matches!(bs[0], Block::Para(_)),
            "it opens on prose, not a heading"
        );
        let heads: Vec<&str> = bs
            .iter()
            .filter_map(|b| match b {
                Block::Heading(t) => Some(*t),
                Block::Para(_) => None,
            })
            .collect();
        assert!(heads.contains(&"WHAT LEAVES THIS TELEVISION"), "{heads:?}");
        assert!(heads.contains(&"WHAT THE LOG MAY NOT CONTAIN"), "{heads:?}");
        // …and nothing that is prose is mistaken for a heading, which is the failure that would
        // silently restyle a sentence as a section title.
        for b in &bs {
            if let Block::Heading(t) = b {
                assert!(t.len() <= HEADING_MAX, "not a heading: {t}");
                assert!(!t.contains('.'), "a sentence read as a heading: {t}");
            }
        }
    }

    /// Every document survives the split — a page whose body is one long paragraph must still
    /// produce exactly one block rather than none.
    #[test]
    fn every_page_produces_at_least_one_block() {
        for p in Page::ALL {
            assert!(!blocks(p.body()).is_empty(), "{p:?} produced no blocks");
        }
    }

    /// **The notice may not claim silence while this build can send.**
    ///
    /// It replaced a grep for `net::post_ca` under `src/telemetry`, which asked "can this build
    /// send at all" — a proxy, and one that could only ever fire once, on the day a sender landed.
    /// It could say nothing about whether the notice DESCRIBED what would be sent, which is the
    /// thing LG requires to be readable in the app and the thing a viewer is relying on.
    ///
    /// Deliberately narrow, because the siblings above already cover the rest: the recipients, the
    /// region and the retention are
    /// [`the_privacy_page_names_the_recipients_region_and_retention`]'s, and the defaults are
    /// [`the_privacy_page_states_the_consent_position_it_can_be_held_to`]'s. What is left here is the two things
    /// neither of them can see — that both consent CATEGORIES are described in the words the
    /// consent screen uses, and that no earlier phrasing claiming silence has survived the sender
    /// landing. `diag::schema::EVENT_SPECS` checks the FIELD-level promise against `PRIVACY.md`;
    /// this is the sofa-readable half of the same claim.
    #[test]
    fn the_notice_cannot_claim_silence_while_this_build_can_send() {
        let p = PRIVACY.to_ascii_lowercase();
        // The two switches, in the words the consent screen offers them in.
        assert!(
            p.contains("crash"),
            "the notice never describes the errors switch"
        );
        assert!(
            p.contains("screens"),
            "the notice never describes the usage switch"
        );
        // Phrasings that were true before a sender existed and would be a false statement now. The
        // full stop on the second is load-bearing: the notice legitimately opens "sends nothing to
        // its developer UNLESS you switch it on", and matching that would fail the honest wording.
        for stale in [
            "no analytics, no telemetry",
            "sends nothing to its developer.",
            "no telemetry of any kind",
            "nothing leaves this television",
        ] {
            assert!(
                !p.contains(stale),
                "the notice still tells the viewer {stale:?}, but this build can send. That notice \
                 is what LG requires to be readable IN the app; it changes in the SAME commit that \
                 gives the app the ability, not afterwards."
            );
        }
    }
}
