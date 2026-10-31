//! **The classic transport's primitives and constants**: the numbers `builtin:classic` is written
//! against (`SB_*`, `BTN_*`, `CTRL_*`, [`ROW_ANCHOR`]), the draws the renderer ([`super::render`]) calls
//! (scrim, title, playbar, clocks, the scrub preview), the three the trailer (`screens/src/detail/trailer.rs`)
//! calls (`draw_scrim`, `draw_title`, `draw_playbar`) and the failure read-out. The bottom tab pills are the
//! renderer's; this file keeps their label list ([`tab_labels`]) and state ([`hud_tab_state`]).
//!
//! The ARRANGEMENT is not here: which control stands where, and each rect, is what the compiled package
//! says (`hud_format::compile`, reported through [`super::geometry`]); `builtin:classic` is the embedded
//! package `fixtures/classic.plxhud.json`. This file holds the primitives it draws with, so a pill is
//! the width [`stand_in_w`] gives it, the row stands on [`ROW_ANCHOR`], and the trailer's three calls
//! keep their output.
//!
//! A draw takes what it paints and the values it reads as arguments, never a process global (the scrub
//! preview, the one that needs the frame's playhead and picks, takes the per-frame
//! [`HudModel`](super::model::HudModel) snapshot), so this file names no player global. The logic it draws
//! ([`super::state`]) and the animation state it reads ([`super::motion`]) are separate modules.
use crate::hud_format::compile::{ClockMode, KnobMode, MarkMode};
use super::geometry::{RowAnchor, GROUP_SCRUB, GROUP_TABS, HUD_TABS};
use super::model::HudModel;
use super::motion::*;
use super::state::*;
use super::HudBand;
use plx_machine::machine::{GroupId, Measure};
use plx_ui::consts::{SCR_H, SCR_W};
use plx_ui::hstack::{Align, HStack, Item};
use plx_ui::theme;
use plx_ui::widgets::{Spinner, StatusKind, StatusOverlay};
use plx_ui::{Env, Painter, Rect, View};
use std::ffi::CString;

/// The HUD widgets draw purely from their own fields and ignore their Env.
pub(super) fn hud_env() -> Env {
    Env::inert()
}

// The now-playing title under the playbar is a HUD *display* title — deliberately larger than
// `theme::size::TITLE`, so it sits OUTSIDE the shared type scale as a documented carve-out (like the
// failure-message caption in `draw_subtitle_message`). Both are media chrome with their own legibility contract and
// both are already well above the couch floor; they're named here rather than left as bare literals.
// The HUD package format's `HUD_TITLE` type rung (`hud_format::schema::SIZE_RUNGS`) reads this very const.
pub(crate) const HUD_TITLE_SZ: i32 = 54;

/// the shared playback clock ([`plx_ui::fmt::clock`]) with the HUD's leading '-' for remaining
pub(crate) fn fmt_time(ns: i64, neg: bool) -> String {
    let c = plx_ui::fmt::clock(ns / 1_000_000);
    if neg {
        format!("-{c}")
    } else {
        c
    }
}

// ---- the numbers classic's package is written against ----
// scrubber (and title, and the bottom tab pills) left margin — the app's own, not a second copy of
// it: this was a literal 90 and so stayed put when `MARGIN_X` moved to the overscan-safe 96
pub(super) const SB_X: f32 = plx_ui::consts::MARGIN_X;
pub const fn sb_w() -> f32 {
    SCR_W - 2.0 * SB_X
}
pub(super) const SB_Y: f32 = SCR_H - 198.0;
pub(super) const SB_H: f32 = 8.0;
/// The radius of the largest thing the bar draws around the playhead, the focused knob's glow (a 34 px
/// disc centred on the track). The caption band counts it as part of the bar: `hud_format::compile`.
pub const KNOB_GLOW_R: f32 = 17.0;
/// Where the clocks' line stands: its top is this far below the TRACK's top edge. [`draw_playbar_look`] draws
/// both clocks there, and `hud_format::compile` reserves the line under the bar from it (`clock_line_h`), so
/// the two cannot drift: a band under the bar starts below the clocks, not on them.
pub const CLOCK_TOP: f32 = 30.0;
/// A clock line's height as a multiple of its rung's size: the room the compiler reserves for it. Inter's
/// ascender plus descender is 1.21 em (`ShippedMeasure::line_h`), and SDL_ttf rounds each to whole pixels, which
/// is 30 px at the clocks' CAPTION rung (24 px; a pinned test holds the ratio above every measure's line).
/// It is a ratio and not the `Measure`'s line height on purpose: the reserved room moves every band above the
/// bar, and classic's rows must not move with the font the host measures with.
pub const CLOCK_LINE: f32 = 1.25;
/// The height the playbar reserves for its clocks' line at rung `size`: [`CLOCK_LINE`] of it, rounded up to
/// whole pixels so no band above the bar lands on a half pixel (LABEL's 32.5 reserves 33, MICRO's 27.5 reserves
/// 28; CAPTION's 30 is whole already).
pub fn clock_line_h(size: i32) -> f32 {
    (size as f32 * CLOCK_LINE).ceil()
}
pub(super) const BTN_S: f32 = 64.0; // right-side control button size (mockup ≈ 68)
pub(super) const BTN_Y: f32 = HudBand::CLASSIC.controls_top_y;

// The control row, exported for `skip_pill` — while a marker is under the playhead the Skip button
// STANDS IN for the two discs, and it has to land on exactly their row and right edge or the
// transport visibly jumps when a segment begins.
/// control-row height (one disc's diameter)
pub const CTRL_H: f32 = BTN_S;
/// control-row top
pub const CTRL_Y: f32 = BTN_Y;
/// control-row right edge — the app's side margin, matching the track-menu panel.
///
/// The mock's `right: 80` put the Subtitles/Audio/`…` discs 16px past the 5% overscan frame, and the
/// track and `…` panels were aligned to the same 80 (`track_menu`/`more_menu`). All three moved onto
/// `MARGIN_X` together on 2026-08-23 — they are one right edge by design, so they take one number.
pub const CTRL_RIGHT: f32 = SCR_W - plx_ui::consts::MARGIN_X;
/// **Where classic's control row hangs**: [`CTRL_RIGHT`], [`CTRL_Y`] and [`CTRL_H`] as the one value a stand-in
/// (Skip, Up Next) and a player menu's default anchor land on. The compiled package reports the same
/// numbers for itself (`HudGeometry::row_anchor`); this is the named source for a reader that holds no
/// package (a menu mounted without an opener, a pin that grades the menus against the row).
pub const ROW_ANCHOR: RowAnchor = RowAnchor { right: CTRL_RIGHT, top: CTRL_Y, h: CTRL_H };
/// width the disc row occupies, the floor for anything replacing it
pub fn ctrl_row_w() -> f32 {
    disc_row().width()
}

/// **The baseline disc row as ONE run**: absent Previous/Next followed by the three always-present
/// discs of [`BTN_S`], `CONTROL_GAP` apart, anchored on [`CTRL_RIGHT`]. The row's width ([`ctrl_row_w`])
/// and its band ([`ctrl_row_hit_rect`]) read this one stack.
fn disc_row() -> HStack<'static> {
    const DISCS: [Item; BTN_N as usize] = disc_items(false);
    disc_stack(&DISCS)
}

/// The disc row's one layout — its gap and its anchor on [`CTRL_RIGHT`] — over either item set.
fn disc_stack(items: &[Item]) -> HStack<'_> {
    HStack::new(items, plx_ui::widgets::CONTROL_GAP, Align::Trailing(CTRL_RIGHT))
}

/// A stand-in pill's side padding: the air between its label and its edge, each side.
const STAND_IN_PAD_X: f32 = 34.0;

/// **The width of a stand-in pill** (Skip, Next Episode): its label's bold advance and the padding
/// each side, never narrower than `floor`. Pure: the HUD layout compiler (`hud_format::compile`, which
/// supplies the floor its package's replaced segment gives) sizes the pill with this one formula.
pub fn stand_in_w(advance: f32, floor: f32) -> f32 {
    (advance + 2.0 * STAND_IN_PAD_X).max(floor)
}

/// The five possible discs, in one right-anchored layout. Absent episode controls contribute
/// neither width nor gaps, so Subtitles, Audio and More never move when the pair appears.
const fn disc_items(pair: bool) -> [Item; BTN_N as usize] {
    [if pair { Item::new(BTN_S) } else { Item::absent() },
     if pair { Item::new(BTN_S).after(2.0 * plx_ui::widgets::CONTROL_GAP) } else { Item::absent() },
     Item::new(BTN_S), Item::new(BTN_S), Item::new(BTN_S)]
}

/// The bottom scrim's height — the transport's dark ground, transparent at its top edge and
/// `theme::scrim_black(0.86)` at the panel bottom. Named because the read-out's placement is graded
/// against it (see [`readout_frame`] and its test) rather than against a second hand-typed 470. The HUD
/// package format bounds a scrim's `extra` and `height` by it (`hud_format::schema`), the same const.
pub(crate) const SCRIM_H: f32 = 470.0;

/// The read-out's frame: **the whole panel**, because the wait is about the whole picture.
///
/// It is deliberately NOT carved down to dodge the transport. The block [`StatusOverlay`] builds is
/// only ~91 px tall, so centred on the panel it spans y≈482…573 — clear of the scrim's top edge at
/// `SCR_H - SCRIM_H` (610) and of the transport's highest ink (the Up Next still at ≈597). The
/// carve-out this replaces (`SCR_H - 340`) centred it at y=370, visibly high, and bought nothing:
/// at 370 the block was already 200 px above a scrim that is fully TRANSPARENT at its top edge
/// anyway. Every "sit above the transport" framing pushes it UP — the un-scrimmed band's own centre
/// is 305 — which is the opposite of the fix the report asked for.
pub(super) fn readout_frame() -> Rect {
    Rect::FULL
}

/// The load / seek / failure read-out. Drawn EVERY player frame, independent of the transport's
/// auto-hide, because its visibility is the PLAYBACK STATE and not the HUD timer.
///
/// It used to live inside the transport's draw, and two things came of that. Historically: `PlaybackState`
/// was published every frame and NOTHING read it, so the initial load drew a live-looking transport
/// at 0:00 / -0:00 and a dead producer drew a fully black screen with no message and no hint that
/// BACK is the way out. Then, once it did read it: in `Error` — which is deliberately not
/// `is_busy()`, hence not covered by `app.rs`'s `|| player::loading()` HUD pin — "Playback failed"
/// disappeared 4.5 s in with the HUD linger, leaving exactly the silent black screen the read-out
/// exists to prevent. A read-out is not transport chrome.
pub fn draw_readout(
    ps: &plx_media::route::PlaybackSession,
    busy: Busy,
    now: u32,
    failure_focus: usize,
    measure: &dyn plx_machine::machine::Measure,
) {
    let Busy::Readout(kind, caption) = busy else {
        return;
    };
    if kind == StatusKind::Failed {
        // The failure is a page read-out (`FailureReadout`): the shared page placement, a
        // constant verdict, one reason, a row of controls. The caption is not drawn here: the
        // verdict line is constant by design, so a viewer who has seen it once recognises it
        // before reading, and the caption's suffix is re-derived as the reason.
        draw_failed_readout(ps, Painter::root(), failure_focus, measure);
        return;
    }
    StatusOverlay::new(readout_frame(), caption, kind)
        .phase(now)
        .draw(&hud_env(), Painter::root());
}

/// The support line's cap top this far above the canvas bottom — inside the safe area's
/// bottom margin band's upper edge, clear of the row by construction, and
/// `the_diagnostics_footer_sits_below_the_row_inside_the_safe_bottom_band` fails if a taller row or
/// note ever meets it.
pub(super) const FOOTER_TOP: f32 = SCR_H - plx_ui::consts::MARGIN_Y - theme::space::XL;

impl FailureReadout {
    fn overlay(&self, labels: &[&'static std::ffi::CStr], focus: Option<usize>) -> StatusOverlay<'_> {
        StatusOverlay::new(Rect::FULL, plx_platform::i18n::msg::widgets_status_failed_c(), StatusKind::Failed)
            .page(self.glyph)
            .reason(&self.reason)
            .row(labels)
            .note(self.note.as_deref())
            .focus(focus)
    }

    fn labels(&self) -> Vec<&'static std::ffi::CStr> {
        self.actions.iter().map(|a| failure_action_label(*a)).collect()
    }

    /// Each control's rect, by row index — the geometry the draw uses.
    pub fn frames(&self, measure: &dyn plx_machine::machine::Measure) -> [Option<Rect>; plx_ui::widgets::STATUS_ROW_MAX] {
        let labels = self.labels();
        self.overlay(&labels, None).row_frames_measured(measure)
    }

    pub(super) fn draw(&self, p: Painter, focus: usize, measure: &dyn plx_machine::machine::Measure) {
        let labels = self.labels();
        self.overlay(&labels, Some(focus.min(labels.len().saturating_sub(1))))
            .draw_measured(&hud_env(), p, measure);
        plx_ui::text_view::TextView::new(self.footer.to_str().unwrap_or(""), theme::size::CAPTION, theme::TEXT_TERTIARY)
            .h(plx_ui::label::HAlign::Center)
            .max_lines(1)
            // the full safe width, not the reason's: the failure code is the line's LAST field
            // and the one support needs, so it must not be the part an ellipsis eats
            .draw(p, Rect::new(plx_ui::consts::MARGIN_X, FOOTER_TOP, SCR_W - 2.0 * plx_ui::consts::MARGIN_X, 0.0));
    }
}

/// **The control row's band at its floor width** ([`ctrl_row_w`]) — the row GROUP's extent only.
///
/// It is not any item's hit region: every item of whatever occupies the row — a disc, the Skip
/// pill, either Up Next button — is placed and registered at its own drawn rect, which the compiled
/// package reports. This one region used to BE the stand-ins' registered stop, which left *Watch
/// Credits* (drawn left of it) unclickable and made a click on *Next Episode* land on item 0,
/// *Watch Credits* (issue #162's audit). It is where a player menu with no opener hangs from.
pub fn ctrl_row_hit_rect() -> Rect {
    let w = ctrl_row_w();
    Rect::new(CTRL_RIGHT - w, CTRL_Y, w, CTRL_H)
}

/// The bottom tab pills' labels: Info always, Chapters only when the item has any; `None` is a
/// tab the row does not carry.
pub(super) fn tab_labels(has_chapters: bool) -> [Option<&'static str>; HUD_TABS] {
    [
        Some(plx_platform::i18n::msg::widgets_player_info()),
        has_chapters.then(plx_platform::i18n::msg::widgets_player_chapters),
    ]
}

/// The tab a `toggle_section`'s section is: the index into [`tab_labels`], the tab cursor and the tab
/// elements. `None` for a section the HUD has no tab for.
pub(super) fn tab_of_section(section: &str) -> Option<i32> {
    match section {
        "info" => Some(0),
        "chapters" => Some(1),
        _ => None,
    }
}

fn draw_failed_readout(
    ps: &plx_media::route::PlaybackSession,
    p: Painter,
    focus: usize,
    measure: &dyn plx_machine::machine::Measure,
) {
    // The GROUND, first: a full-bleed opaque black, one quad. Without it the read-out stood on
    // whatever the video plane happened to be holding: `app.rs` clears the graphics plane to alpha
    // 0 on the player route, and the transport is not drawn at all under a failure. "The plane is
    // black by the time an error is up" is usually true and not always — `busy_surface` resolves
    // Error to Failed for BOTH values of `seen_frame`, i.e. a mid-playback failure over a held
    // frame is a state the code contemplates.
    p.rrect(Rect::FULL, 0.0, 0.0, theme::scrim_black(1.0));
    FailureReadout::now(ps).draw(p, focus, measure);
}

// (`scrub_hit` stood here, and then `icon_hit` and `tab_hit_rect` beside it: the raw `(cx,cy) -> Option`
// hit-testers. The bar, the discs and the pills are registered `Stop`s now, at the rects the compiled
// package reports, so the shared hit map answers "is the pointer on it" and the screen asks only "where
// along it" — `HudGeometry::scrub_frac`, the half that was always x-only. Having none left is also what
// makes `ci/check-deps.sh`'s `hittest` gate meaningful rather than merely satisfied: there is no raw
// hit-tester left for `app/` to call.)

// ---- the rail carries NO marks ------------------------------------------------------------
// Nothing is drawn on the scrubber but the track, the watched fill and the playhead. Two kinds of
// mark have been tried here and both were removed, for the same reason and by the same judgement:
//
//   * chapter-boundary ticks — the rail reads cleaner as one continuous bar. The chapter LIST is
//     the affordance (`appkit/chapters_panel.rs`, off `metadata::playing_chapters()`).
//   * intro/credits segment bands — removed 2026-08-04. They were `RAIL_MARKER`, white at 0.42
//     against a 0.20 track, so a credits band on a feature film was a bright unlabelled patch
//     floating near the right end of an otherwise empty rail. Reviewed cold on a screenshot it
//     read as a RENDERING ARTIFACT, not as information, which is a complete failure of the thing.
//
// Neither loses anything: the Skip Intro / Skip Credits pill (`appkit/skip_pill.rs`) is driven from
// the very same `metadata::playing_markers()` and appears exactly when a marker is reachable, so
// the band was decoration duplicating a control that already announces itself. Do not re-add
// either as a "cheap win" — the marker data is already in memory, which is precisely what makes
// drawing it tempting and still wrong.

/// draw a clock string centred on `cx` as ONE label box, clamped so it stays inside [lo, hi].
/// (The old last-':'-anchor read visibly lopsided once the clock grew to H:MM:SS — "1:05" left of
/// the knob vs "53" right.) The box width is measured on a same-shape template with every digit
/// as '0', so the box — and the returned extents — stay stable while digits tick instead of
/// wobbling with proportional digit widths. Returns the label's (left, right) x extents.
fn draw_clock(
    p: Painter,
    text: &str,
    cx: f32,
    y: f32,
    sz: i32,
    col: [f32; 4],
    lo: f32,
    hi: f32,
    measure: &dyn plx_machine::machine::Measure,
) -> (f32, f32) {
    let template: String = text
        .chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect();
    let w = measure.width_str(&template, sz, true);
    let half = w * 0.5;
    let cx = cx.clamp(lo + half, (hi - half).max(lo + half));
    if let Ok(cs) = CString::new(text) {
        p.text(cs.as_ptr(), cx, y, sz, col, 1, 1);
    }
    (cx - half, cx + half)
}

/// An open Info/Chapters panel owns input; its retained HUD tab indicates mode only.
/// Returning to the transport restores its actionable tab focus without losing that cursor.
pub(super) fn hud_tab_state(transport: bool, focus: GroupId, tab: i32, index: i32) -> (bool, bool) {
    let current = tab == index;
    (transport && focus == GROUP_TABS && current, !transport && current)
}

/// The transport's dark ground: transparent at its top edge, `theme::scrim_black(0.86)` at the
/// panel bottom, [`SCRIM_H`] tall. Drawn under every transport, the player's and the trailer's.
pub fn draw_scrim(p: Painter) {
    draw_scrim_at(p, Rect::new(0.0, SCR_H - SCRIM_H, SCR_W, SCRIM_H));
}

/// [`draw_scrim`] over any rect: the compiled package's scrim (`CompiledState::scrim`) is classic's
/// rect for `builtin:classic`, and a package's own for another.
pub fn draw_scrim_at(p: Painter, rect: Rect) {
    p.rect(
        rect,
        0.0,
        theme::scrim_black(0.0),
        theme::scrim_black(0.86),
        0.0,
    );
}

/// The line over a transport's display title. Non-owning `*const c_char`, the `Label` rule
/// (`ui/CLAUDE.md`): keep the `CString` (or the session's fixed buffer) alive across the draw.
#[derive(Clone, Copy, Debug)]
pub enum Kicker {
    /// an episode's own address and name ("S1, E1 · Pilot"): primary ink, bold — it IS the
    /// episode's title, with the show's name as the display title under it
    Episode(*const std::os::raw::c_char),
    /// a context label (`metadata::context_label`: an extra's kind, a movie's route ctxline):
    /// secondary ink, regular — it only says what the display title is
    Context(*const std::os::raw::c_char),
}

/// The trailer's unbounded title block (it has no player control row).
pub fn draw_title(p: Painter, kicker: Kicker, title: *const std::os::raw::c_char) {
    let (text, ink, bold) = match kicker {
        Kicker::Episode(text) => (text, theme::TEXT_PRIMARY, 1),
        Kicker::Context(text) => (text, theme::TEXT_SECONDARY, 0),
    };
    p.text(text, SB_X, SCR_H - 312.0, theme::size::CAPTION, ink, 0, bold);
    p.text(title, SB_X, SCR_H - 278.0, HUD_TITLE_SZ, theme::TEXT_PRIMARY, 0, 1);
}

/// Where the player title's two lines stand: `x` the left edge both start at, and the y each line is
/// drawn from. `Default` is classic's (`SB_X`, `SCR_H - 312`, `SCR_H - 278`); the renderer builds one from the
/// compiled title cell and takes classic's values for a line the package does not place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TitleLook {
    pub x: f32,
    pub kicker_y: f32,
    pub title_y: f32,
    /// the kicker's size (`text.size` of `title.kicker`); classic's is the caption rung
    pub kicker_size: i32,
    /// the title's size (`text.size` of `title.main`); classic's is [`HUD_TITLE_SZ`]
    pub title_size: i32,
}

impl Default for TitleLook {
    fn default() -> Self {
        Self {
            x: SB_X,
            kicker_y: SCR_H - 312.0,
            title_y: SCR_H - 278.0,
            kicker_size: theme::size::CAPTION,
            title_size: HUD_TITLE_SZ,
        }
    }
}

/// **The player's title in the cell a [`TitleLook`] names** (the compiled package's title cell, its text ops' rects). It is always
/// contained before `right`: while playing it is ellipsised; while paused the full line glides once to reveal its
/// end, then rests.
pub(super) fn draw_player_title_at(
    p: Painter,
    look: TitleLook,
    kicker: Kicker,
    title: *const std::os::raw::c_char,
    right: f32,
    measure: &dyn Measure,
    paused: bool,
    clock: &plx_ui::marquee::Clock,
) {
    let TitleLook { x: left, kicker_y, title_y, kicker_size, title_size } = look;
    let width = (right - left).max(0.0);
    let _clip = plx_ui::screen::ClipScope::open_in(p, Rect::new(left, 0.0, width, SCR_H));
    let (text, ink, bold) = match kicker {
        Kicker::Episode(text) => (text, theme::TEXT_PRIMARY, true),
        Kicker::Context(text) => (text, theme::TEXT_SECONDARY, false),
    };
    if !text.is_null() {
        let label = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
        let fitted = measure.fit_line(&label, width, kicker_size, bold);
        p.text(fitted.as_ptr(), left, kicker_y, kicker_size, ink, 0, i32::from(bold));
    }
    if title.is_null() {
        clock.release();
        return;
    }
    let full = unsafe { std::ffi::CStr::from_ptr(title) };
    let label = full.to_string_lossy();
    let full_width = measure.width(full, title_size, true);
    if paused && full_width > width {
        // A show's display title can stay the same across episodes; the kicker is
        // part of the visible identity and must restart the reveal when it changes.
        let kicker = if text.is_null() { "" } else { unsafe { std::ffi::CStr::from_ptr(text) }.to_str().unwrap_or("") };
        let key = format!("{kicker}\u{1}{label}");
        let elapsed = clock.read(&key) as f32;
        let offset = plx_ui::marquee::TITLE.reveal_once(elapsed, full_width, width);
        if offset < full_width - width {
            if elapsed < plx_ui::marquee::TITLE.hold_ms {
                plx_machine::idle::wake();
            } else {
                plx_machine::idle::invalidate();
            }
        }
        if offset > 0.0 {
            // an Arabic or Hebrew title's end is on its left: it moves right instead
            let rtl = plx_gfx::text::is_rtl_paragraph(&label);
            let x = plx_ui::marquee::TITLE.reveal_once_at(elapsed, full_width, width, rtl);
            p.text(title, left + x, title_y, title_size, theme::TEXT_PRIMARY, 0, 1);
            return;
        }
    } else {
        clock.release();
    }
    let fitted = measure.fit_line(&label, width, title_size, true);
    p.text(fitted.as_ptr(), left, title_y, title_size, theme::TEXT_PRIMARY, 0, 1);
}

/// How the playhead is drawn — see [`draw_playbar`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Knob {
    /// the scrubber holds the focus ring: a glowing knob
    Focused,
    /// a scrub preview is being dragged with the ring elsewhere: a plain knob
    Held,
    /// neither: a thin tick
    Tick,
}

/// Everything [`draw_playbar`] draws from, resolved by its caller. A value rather than a read of
/// the transport's globals inside the draw, because TWO surfaces draw this bar and they resolve it
/// differently: the player route from its own scrub gesture, seek target and focus ring
/// ([`Playbar::live`]), and the detail page's full-trailer transport from a preview session that
/// has none of the three.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Playbar {
    /// the DISPLAYED playhead (a seek target, a scrub preview or the published position)
    pub pos_ns: i64,
    pub dur_ns: i64,
    pub knob: Knob,
    /// the state read-out beside the elapsed clock, from [`transport_mark`]
    pub mark: TransportMark,
    /// drives the inline spinner's rotation
    pub now: u32,
    /// how far the knob clock has grown, 0 (resting, [`theme::size::CAPTION`]) to 1
    /// ([`crate::scrub_preview::CLOCK_GROWN_SZ`]), while a scrub shows no card
    /// ([`crate::scrub_preview::Motion::clock_grow`]). The two sizes are CROSSFADED on it
    /// ([`crate::scrub_preview::clock_faces`]); a size is never tweened.
    pub clock_grow: f32,
}

impl Playbar {
    /// The player route's bar: the live scrub preview and seek target over the published
    /// playhead, the knob from the HUD's own focus ring, and the four-state read-out.
    pub(super) fn live(
        model: &HudModel,
        busy: Busy,
        focus: GroupId,
        since_play_ms: Option<u32>,
        now: u32,
    ) -> Self {
        // The scrub preview, the seek target and the published playhead are ONE sample each, taken
        // with the frame's model: the state read-out takes its travel direction from the difference
        // between the displayed position and the PUBLISHED one, and two loads of a live atomic can
        // straddle a tick.
        let scrub = model.scrub_ns;
        // while a seek is loading, freeze the playhead at the target (no wobble through the reopen);
        // else follow the live scrub preview, else the real playhead.
        let livepos = model.playpos_ns;
        let dispos = display_pos_ns(model.loading, model.seek_display_ns, scrub, livepos);
        let knob = if focus == GROUP_SCRUB {
            Knob::Focused
        } else if scrub >= 0 {
            Knob::Held
        } else {
            Knob::Tick
        };
        // Gated on `busy`, not on `loading()`: with `loading()` the transport lit the same spinner
        // the centred read-out was already showing, for the whole of every load AND every seek.
        let paused = model.paused;
        Self {
            pos_ns: dispos,
            dur_ns: model.duration_ns,
            knob,
            mark: transport_mark(paused, busy, scrub >= 0, dispos, livepos, since_play_ms),
            now,
            clock_grow: 0.0,
        }
    }
}

/// What a playbar looks like where a package places it: the track's rect, the fill and track inks, the
/// clocks' size, and the knob, clock and mark options. `Default` is classic's (`SB_*`, `TEXT_PRIMARY`, `RAIL_TRACK`, `CAPTION`), so
/// [`draw_playbar`] is [`draw_playbar_look`] over it and the trailer's frame cannot move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaybarLook {
    pub track: Rect,
    pub fill: [f32; 4],
    pub track_ink: [f32; 4],
    pub clock_size: i32,
    /// `tick`: a thin tick at rest. `always`: the held knob at rest too.
    pub knob: KnobMode,
    /// `follow_head`: the elapsed clock under the playhead. `ends`: at the bar's start, with the
    /// remaining clock at its end.
    pub clocks: ClockMode,
    /// `after_elapsed`: the state glyph beside the elapsed clock. `busy_only`: the spinner alone.
    pub mark: MarkMode,
}

impl Default for PlaybarLook {
    fn default() -> Self {
        Self {
            track: Rect::new(SB_X, SB_Y, sb_w(), SB_H),
            fill: theme::TEXT_PRIMARY,
            track_ink: theme::RAIL_TRACK,
            clock_size: theme::size::CAPTION,
            knob: KnobMode::Tick,
            clocks: ClockMode::FollowHead,
            mark: MarkMode::AfterElapsed,
        }
    }
}

/// The playbar: scrubber, playhead knob, elapsed/remaining clocks and the state read-out. The
/// player HUD and the detail page's full-trailer transport both draw it, so the two cannot drift.
pub fn draw_playbar(p: Painter, bar: Playbar, measure: &dyn plx_machine::machine::Measure) {
    draw_playbar_look(p, bar, PlaybarLook::default(), measure);
}

/// [`draw_playbar`] over a [`PlaybarLook`]: the compiled package's bar (`DrawOp::Playbar`).
pub fn draw_playbar_look(p: Painter, bar: Playbar, look: PlaybarLook, measure: &dyn plx_machine::machine::Measure) {
    let e = hud_env();
    let white = look.fill;
    let dim = theme::TEXT_SECONDARY;
    let track = look.track_ink;
    let sx = look.track.x;
    let sw = look.track.w;
    let sy = look.track.y;
    let sh = look.track.h;
    let dispos = bar.pos_ns;
    let dur = bar.dur_ns;
    let now = bar.now;
    let frac = if dur > 0 {
        (dispos as f64 / dur as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    p.rect(Rect::new(sx, sy, sw, sh), sh * 0.5, track, track, 0.0);
    let fw = (sw as f64 * frac) as f32;
    if fw > sh * 0.5 {
        p.rrect(Rect::new(sx, sy, fw, sh), sh * 0.5, 0.0, white);
    } else if fw > 0.0 {
        p.rrect(Rect::new(sx, sy, fw, sh), fw * 0.5, 0.0, white);
    }
    // playhead: a focus-glowing knob when the scrubber is focused, a plain knob while scrubbing,
    // else a thin tick.
    let hx = sx + fw;
    let cy = sy + sh * 0.5;
    if bar.knob == Knob::Focused {
        let glow = [1.0f32, 1.0, 1.0, 0.22];
        p.rect(
            Rect::new(hx - KNOB_GLOW_R, cy - KNOB_GLOW_R, 2.0 * KNOB_GLOW_R, 2.0 * KNOB_GLOW_R),
            KNOB_GLOW_R,
            glow,
            glow,
            0.0,
        );
        p.rect(
            Rect::new(hx - 11.0, cy - 11.0, 22.0, 22.0),
            11.0,
            white,
            white,
            0.0,
        );
    } else if bar.knob == Knob::Held || look.knob == KnobMode::Always {
        p.rect(Rect::new(hx - 9.0, cy - 9.0, 18.0, 18.0), 9.0, white, white, 0.0);
    } else {
        p.rect(
            Rect::new(hx - 1.5, sy - 4.0, 3.0, sh + 8.0),
            0.0,
            white,
            white,
            0.0,
        );
    }

    // elapsed under the playhead (':' centered on the knob, clamped to the bar); remaining at the
    // right — hidden once the moving elapsed label would overlap it (near the end of the movie).
    let ty = sy + CLOCK_TOP;
    // A grown knob clock keeps its BASELINE, so it grows upward and never into the remaining clock's
    // line. It is a CROSSFADE of two ladder rungs (the resting CAPTION and the one above it), each
    // laid out at its own size, never a tweened size or a scaled texture; the extents the rest of
    // the line clears are the union of the faces drawn.
    let elapsed = fmt_time(dispos, false);
    // `ends` seats the elapsed clock at the bar's start: centring it on the bar's left edge lets the
    // clamp inside `draw_clock` put its left edge there.
    let clock_x = if look.clocks == ClockMode::Ends { sx } else { hx };
    let (mut el_l, mut el_r) = (f32::INFINITY, f32::NEG_INFINITY);
    for (sz, face_a) in crate::scrub_preview::clock_faces(look.clock_size, bar.clock_grow).into_iter().flatten() {
        let ety = plx_gfx::text::baseline_y(sz, 1, look.clock_size, 1, ty);
        let (l, r) = draw_clock(p.alpha(face_a), &elapsed, clock_x, ety, sz, white, sx, sx + sw, measure);
        el_l = el_l.min(l);
        el_r = el_r.max(r);
    }
    let rem = fmt_time(dur - dispos, true);
    // MEASURE the label, never estimate it. `chars * CAPTION * 0.52` was an Arial-calibrated
    // constant guarding a real behaviour — the remaining clock hides before the moving elapsed
    // clock can reach it — and it only ever worked because it over-estimated: under Arial it
    // returned ~99.8px against a true ~85px. Under the shipped Inter that margin had already
    // halved, and freezing tabular figures (which the clock's own template idiom requires, see
    // `draw_clock`) widens numerals further, leaving about a pixel of slack. At that point the
    // guard stops guarding and the two clocks can overlap near the end of a long item.
    // Same '0'-template `draw_clock` uses, so the two agree by construction: with tabular figures
    // the template's width IS the real string's width at every tick.
    let rem_tmpl: String = rem
        .chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect();
    let rem_w = measure.width_str(&rem_tmpl, look.clock_size, true);
    let rem_l = sx + sw - rem_w;
    let rem_shown = el_r + 20.0 < rem_l;
    if rem_shown {
        if let Ok(cs) = CString::new(rem.as_str()) {
            p.text(cs.as_ptr(), sx + sw, ty, look.clock_size, dim, 2, 0);
        }
    }
    // transport state indicator just past the elapsed clock — the four-state READ-OUT resolved by
    // [`transport_mark`] (rewind / pause / fast-forward / play, plus the narrowed spinner), and
    // NOTHING while playing steadily. A read-out, not an action toggle: nothing here is focusable
    // or hit-tested, and the control row stays the three discs it has always been.
    // Gated on `busy`, not on `loading()`: with `loading()` the transport lit the same spinner the
    // centred read-out was already showing, for the whole of every load AND every seek. Centered on
    // the clock's line box; drops to the clock's LEFT when the right side is against the remaining
    // label / screen edge.
    let mark = match look.mark {
        MarkMode::BusyOnly if bar.mark != TransportMark::Working => TransportMark::None,
        _ => bar.mark,
    };
    if mark != TransportMark::None {
        // pause bars under-fill their viewBox (14/24 tall) — a 30px box renders ~17px of ink,
        // matching the CAPTION clock's cap height so the glyph reads as the label's size. The two
        // travel marks are drawn to that SAME 14-unit band (see `Icon::Rewind`), which is what lets
        // one box serve the whole family without the slot changing weight as the state flips.
        let isz = 30.0f32;
        // The odd member of the family — `play.svg` is authored to 16 units where the other three
        // are 14 — is corrected by asking `icons::band` rather than by a constant here. See that
        // function: the metric is a property of the asset, and this was the second screen to
        // transcribe one out of an SVG by hand.
        // **The gap is measured to the INK, not to the box.** Every member of this family carries a
        // different left bearing inside its 24-unit viewBox — pause's bars open at x=7, play's
        // triangle at x=6, the travel marks at x=2.6 — so ONE box origin gives each state a
        // visibly different gap after the clock, and the slot appears to twitch as the state flips.
        // Measuring to the ink makes the gap the eye sees a single number. It is also what the old
        // spacing really was: a box placed 14px out put pause's ink at 14 + 7/24*30 ~= 23px, which
        // is the gap that read as too wide. The bearings come from `icons::ink_x`, not from a table
        // here — they are the asset's, and this screen was the second place to copy them out.
        const GAP: f32 = 14.0;
        let glyph = match mark {
            TransportMark::Pause => Some(plx_ui::icons::Icon::Pause),
            TransportMark::Play => Some(plx_ui::icons::Icon::Play),
            TransportMark::Rewind => Some(plx_ui::icons::Icon::Rewind),
            TransportMark::FastForward => Some(plx_ui::icons::Icon::FastForward),
            TransportMark::Working | TransportMark::None => None,
        };
        let ink = glyph.map_or((0.0, 1.0), plx_ui::icons::ink_x);
        let icy = ty + plx_gfx::text::text_height(look.clock_size, 1) * 0.5; // vertical center of the clock line
                                                                                // scaled so every member of the family lands the SAME height of ink in this one box
        let bs = glyph.map_or(isz, |g| {
            isz * plx_ui::icons::band(plx_ui::icons::Icon::Pause)
                / plx_ui::icons::band(g)
        });
        // **Rewind sits to the LEFT of the clock; everything else to the right.** The mark points
        // the way the playhead is travelling, so `<<` after the time would point back at the number
        // it is leaving. The right-hand placement still falls back to the left when the remaining
        // label or the screen edge crowds it, which is the case this branch was originally for.
        let need = (ink.1 - ink.0) * bs + 6.0;
        let room_right = el_r + GAP + need < if rem_shown { rem_l - 8.0 } else { sx + sw };
        let on_left = mark == TransportMark::Rewind || !room_right;
        // Placed by ink on whichever side it lands: the trailing edge sits GAP before the clock's
        // left, or the leading edge GAP after the clock's right.
        let bx = if on_left {
            el_l - GAP - ink.1 * bs
        } else {
            el_r + GAP - ink.0 * bs
        };
        match glyph {
            Some(id) => {
                plx_ui::icons::draw(p, id, Rect::new(bx, icy - bs * 0.5, bs, bs), white)
            }
            // placeholder-exempt: the transport's inline busy mark on a button (R_INLINE), reported by the playback pipeline, not a content stand-in
            None => Spinner::new(bx + isz * 0.5, icy, Spinner::R_INLINE)
                .phase(now)
                .tint(white)
                .draw(&e, p),
        }
    }
}

/// The bar's x for a position `pos_ns` of `dur_ns`, clamped to the bar's ends.
fn bar_x(pos_ns: i64, dur_ns: i64) -> f32 {
    SB_X + (sb_w() as f64 * (pos_ns as f64 / dur_ns.max(1) as f64).clamp(0.0, 1.0)) as f32
}

/// **The scrub preview** ([`crate::scrub_preview`]): while a live scrub preview is up, the still
/// at the knob's position, floated above the knob; after it, the same still fading out. Draws
/// nothing for a part with no BIF index, and nothing until the first still lands.
pub(super) fn draw_scrub_preview(p: Painter, ps: &plx_media::route::PlaybackSession, model: &HudModel, row: &mut HudMotion, bar: Playbar) {
    use crate::scrub_preview::{card_rect, card_size, draw, scaled_about, still_ms, PosterStills, Stills};
    let mut stills = PosterStills {
        sid: plx_media::route::item_sid(plx_media::route::cur_sid(ps)).raw(),
        part: plx_media::route::cur_part_id(ps),
    };
    let show = if model.scrub_ns < 0 {
        row.preview.motion.exit(bar.now)
    } else if bar.dur_ns <= 0 {
        None
    } else {
        let generation = plx_media::route::sess(ps);
        let target = still_ms(bar.pos_ns);
        let shown = row.preview.step((stills.part, &generation), target, bar.now, |ms| stills.look(ms));
        let absent = row.preview.absent();
        row.preview.motion.observe(shown, target, absent, bar_x(bar.pos_ns, bar.dur_ns), bar.now)
    };
    let Some(sh) = show else { return };
    let Some((tex, tw, th)) = stills.tex(sh.ms) else { return };
    // In-place crossfade only between two resident stills of the same aspect; otherwise the swap is
    // immediate (the card's width would have to jump mid-fade).
    let prev = sh.prev.and_then(|(old, t)| {
        let (otex, ow, oh) = stills.tex(old)?;
        ((ow / oh - tw / th).abs() < 0.01).then_some((otex, t))
    });
    let (w, _) = card_size(tw, th);
    let rest = card_rect(sh.knob_x, SB_X, sb_w(), SB_Y, w);
    let r = scaled_about(rest, sh.knob_x, SB_Y + SB_H * 0.5, sh.scale);
    draw(p.alpha(sh.alpha), r, tex, tw, th, prev);
}
