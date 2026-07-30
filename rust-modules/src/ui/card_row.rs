//! `CardRow` — the shared animated poster-shelf component.
//!
//! Extracts the home shelf's per-row animation state (a focus-scale [`Spring`] per cell + a
//! horizontal scroll spring) and its tile rendering (poster + big glow focus ring + resume bar +
//! centered title) into ONE reusable widget, so the home `Grid` and the detail Related row are
//! literally the same component — [`RowStyle::HOME`] is the single source of the shelf's motion and
//! geometry that both screens read.
//!
//! What stays per-caller is only the x/`base_y` positioning loop, because home threads `env.sp` and
//! splits the loop across the `Grid`'s two-pass **cross-row** focused-last z-order (invariant #3 in
//! `ui/CLAUDE.md`) — which a lone detail row doesn't have. So `CardRow` deliberately owns spring
//! state + the leaf tile draws, and NEVER draws a whole row or applies an in-row focused-last pass
//! on a caller's behalf. It's decoupled from any focus globals / catalog: focus is a plain
//! `Option<usize>`, the item art is a [`Art`] the caller supplies.
use crate::ui::consts::*;
use crate::ui::theme;
use crate::ui::widgets::{card, Art};
use crate::ui::{Painter, Rect, Spring};
use std::os::raw::c_char;

pub(crate) const MAX_ROW_ITEMS: usize = 24; // == home MAX_ITEMS

/// A card row's motion + geometry. [`RowStyle::HOME`] is the single value both the home grid and the
/// detail Related row pass, so the two rows are indistinguishable in look and animation.
#[derive(Clone, Copy)]
pub(crate) struct RowStyle {
    pub w: f32,
    pub h: f32,
    pub gap: f32,
    pub margin_x: f32,
    pub radius: f32,
    pub focus_scale: f32,
    pub k_scale: f32,
    pub k_scroll: f32,
    /// Circular tiles (cast headshots, who's-watching avatars) vs rounded-rect posters. A circle is
    /// just a tile drawn at `radius = width/2`; the shared springs/scroll/ring are identical.
    pub circular: bool,
    /// How many lines the focused tile's under-TITLE may wrap to before it elides. `1` is the
    /// original single elided run every shelf drew; `2` word-wraps instead, which is what a shelf of
    /// real Plex titles needs — "Wallace & Gromit: The Curse of the Were-Rabbit" under a 250px card
    /// loses two thirds of itself to a one-line elide. Raising it is safe because
    /// [`TileLabel::height`] derives the band a screen must reserve from this very field.
    pub title_lines: usize,
}
impl RowStyle {
    /// The home shelf's portrait-poster row: 1.09 focus pop (matches `Home Screen.dc.html`), the big
    /// glow ring, animated scroll. The `ui::press` click dips a focused card from here back toward
    /// `scale(1.0)`, so the pop needs to be large enough for that press-in to read.
    pub(crate) const HOME: RowStyle = RowStyle {
        w: CARD_W,
        h: CARD_H,
        gap: GAP,
        margin_x: MARGIN_X,
        radius: theme::CARD_RING_RAD,
        focus_scale: 1.09,
        k_scale: K_SCALE,
        k_scroll: K_SCROLL,
        circular: false,
        title_lines: 1,
    };
    /// Detail "Cast & Crew": circular headshots, a gentle pop, the tight strip ring. Same motion as
    /// HOME (spring magnification + scroll), so cast animates like the poster shelves.
    pub(crate) const CAST: RowStyle = RowStyle {
        w: 190.0,          // = detail CAST_D
        h: 190.0,
        gap: 40.0,         // w+gap = 230 = detail CAST_SLOT (per-member pitch)
        margin_x: MARGIN_X,
        radius: 95.0, // = w/2 (circle); draw_* recomputes per-rect anyway when `circular`
        focus_scale: 1.06,
        k_scale: K_SCALE,
        k_scroll: K_SCROLL,
        circular: true,
        title_lines: 1,
    };
    /// "Who's watching" profile pictures: big circular avatars with a clear pop. Centered by the
    /// caller (a short roster), so the scroll spring stays put unless the row overflows.
    ///
    /// The pop is much larger than the shelves' 1.09 **on purpose**: a poster shelf marks its focus
    /// against close, hard-edged neighbours, but a lone row of widely-spaced circles has no such
    /// reference and the same 9% read as "nothing is selected". 1.18 still leaves 28px between
    /// adjacent tiles at full pop (slot 292 − 264), and `profiles::NAME_DY` derives the name band's
    /// drop from this number so the label can never collide with the popped circle.
    pub(crate) const PROFILES: RowStyle = RowStyle {
        w: 220.0,
        h: 220.0,
        gap: 72.0,
        margin_x: MARGIN_X,
        radius: 110.0,
        focus_scale: 1.18,
        k_scale: K_SCALE,
        k_scroll: K_SCROLL,
        circular: true,
        title_lines: 1,
    };
    /// ring focus scalar denominator — `(s-1)/ring_denom` maps scale∈[1, focus_scale] to [0, 1].
    #[inline]
    fn ring_denom(&self) -> f32 {
        self.focus_scale - 1.0
    }
    /// Corner radius for a tile of size `rect` at radius-scale `s`: half-width for a circle, else the
    /// style's fixed radius scaled with the focus pop.
    #[inline]
    fn tile_radius(&self, rect: Rect, s: f32) -> f32 {
        if self.circular {
            rect.w * 0.5
        } else {
            self.radius * s
        }
    }
}

/// Per-row animation state: a focus-scale spring per cell + the horizontal scroll spring. `Copy` so
/// a grid can hold `[CardRow; N]` and copy-init it.
#[derive(Clone, Copy)]
pub(crate) struct CardRow {
    scale: [Spring; MAX_ROW_ITEMS],
    /// The ONE spring every cell PAST the array shares — see [`CardRow::scale`]. A cast row is
    /// routinely 60 names long, so "the tiles past the 24th simply don't animate" is not a corner
    /// case; it is most of the Cast & Crew shelf, and the crew now lives at its far end.
    overflow: Spring,
    /// Which cell holds focus (`-1` none), so an overflow cell can claim `overflow` while its
    /// neighbours — which read the same spring — stay at rest.
    focus: i32,
    scroll_x: Spring,
    lift: Spring,
    pub base_y: f32,
}
impl CardRow {
    pub(crate) const fn new() -> Self {
        CardRow {
            scale: [Spring::at(1.0); MAX_ROW_ITEMS],
            overflow: Spring::at(1.0),
            focus: -1,
            scroll_x: Spring::at(0.0),
            lift: Spring::at(0.0),
            base_y: 0.0,
        }
    }
    /// Step every cell's scale spring (all `MAX_ROW_ITEMS` every frame — invariant #10) toward
    /// `focus_scale` for the focused cell else 1.0; then, ONLY when this row is focused, glide the
    /// scroll spring *minimally*: scroll just far enough that the focused cell (plus one gap of
    /// breathing room) sits fully inside the viewport, and not at all when it already does — no
    /// slot pinning, so entering a row never shuffles tiles that were already visible.
    /// `focused == None` freezes the scroll (a non-focused row holds its position — exactly the
    /// home shelf's behavior).
    pub(crate) fn update(&mut self, n: usize, focused: Option<usize>, sty: &RowStyle, dt: f32) {
        self.focus = focused.map(|f| f as i32).unwrap_or(-1);
        for (i, sp) in self.scale.iter_mut().enumerate() {
            let target = if focused == Some(i) { sty.focus_scale } else { 1.0 };
            sp.step(target, sty.k_scale, dt);
        }
        // the shared overflow spring pops only while focus is actually out past the array
        let ot = if focused.is_some_and(|f| f >= MAX_ROW_ITEMS) { sty.focus_scale } else { 1.0 };
        self.overflow.step(ot, sty.k_scale, dt);
        if let Some(fc) = focused {
            if n > 0 {
                let want = scroll_into_view(self.scroll_x.pos, fc, n, sty.w, sty.gap, SCR_W - 2.0 * sty.margin_x);
                self.scroll_x.step(want, sty.k_scroll, dt);
            }
        }
        self.lift.step(heading_clearance(focused, sty, self.scroll_x.pos), sty.k_scale, dt);
    }
    /// Cell `i`'s live focus-pop scale. Cells inside the spring array own a spring each; every cell
    /// PAST it shares `overflow`, and only the focused one reads it — otherwise the whole tail of a
    /// long row would pop together. The cost of sharing is that stepping between two overflow cells
    /// swaps the pop instantly instead of crossfading; the alternative (this used to clamp the index,
    /// so an overflow cell read cell 23's spring) was a focused tile with NO pop, NO lifted shadow
    /// and NO ring — the entire focus treatment is derived from this number.
    #[inline]
    pub(crate) fn scale(&self, i: usize) -> f32 {
        if i < MAX_ROW_ITEMS {
            self.scale[i].pos
        } else if self.focus == i as i32 {
            self.overflow.pos
        } else {
            1.0
        }
    }
    #[inline]
    pub(crate) fn scroll_x(&self) -> f32 {
        self.scroll_x.pos
    }
    /// How far this row's heading must rise, live — see [`heading_clearance`] for the rule and
    /// [`CardRow::update`] for the spring that holds it there.
    #[inline]
    pub(crate) fn lift(&self) -> f32 {
        self.lift.pos
    }
}

/// THE clamp-into-view core every scroller shares — uniform slots ([`scroll_into_view`]),
/// variable-width season tabs, and the home grid's vertical spring: the nearest scroll in
/// `[0, max]` at which the target band is fully revealed. `lo`/`hi` are the scrolls where the
/// item's trailing/leading edge (plus breathing room) just fits; `cur` already inside ⇒ returned
/// unchanged (no pinning).
pub(crate) fn reveal(cur: f32, lo: f32, hi: f32, max: f32) -> f32 {
    cur.clamp(lo.min(hi), hi).clamp(0.0, max)
}

/// The minimal scroll-into-view rule for uniform-slot strips (home shelves, detail
/// Related/Cast/episodes, the profiles roster): scroll only when the focused item — padded by one
/// `gap` of breathing room (which also covers the focus pop) — would clip the viewport `vw`,
/// clamped to the content range so short rows never over-scroll. (Season tabs and the vertical
/// grid have non-uniform geometry and feed their own edges straight into [`reveal`].)
pub(crate) fn scroll_into_view(cur: f32, fc: usize, n: usize, w: f32, gap: f32, vw: f32) -> f32 {
    let slot = w + gap;
    let max_sx = (n as f32 * slot - gap - vw).max(0.0);
    let lo = fc as f32 * slot + w + gap - vw; // right edge (+gap) on screen
    let hi = fc as f32 * slot - gap; // left edge (−gap) on screen
    reveal(cur, lo, hi, max_sx)
}

/// How far a row's heading must rise to stay clear of the focused tile — the ONE rule home hub
/// titles and the detail strip headings ("Related", "Cast & Crew") share. A popped tile's focus
/// glow spills past its top edge and washes over the heading above it, but only while that tile is
/// near the row's left edge (under the heading), so this is the FULL pop height tapered by a
/// proximity ramp. The first TWO slots need the whole clearance — a heading plus the glow's spill
/// reaches past slot 1, and a partial lift there let the glow climb into the title's descender
/// line; the ramp tapers from slot 2 out.
///
/// It is a **destination, not a readout**: the height comes from the style's `focus_scale`, never
/// from a live scale spring. Multiplying by the pop's current magnitude tied the heading to the
/// magnification, so it rode every selection change down and back up — and worse, the frame focus
/// moved the incoming cell's spring was still at rest, so the heading dropped its whole lift in
/// one frame and sprang back. [`CardRow`] holds the result on its own spring, so the heading
/// simply STAYS up for as long as something is under it and settles once when nothing is.
fn heading_clearance(focused: Option<usize>, sty: &RowStyle, scroll: f32) -> f32 {
    let Some(c) = focused else {
        return 0.0;
    };
    let x = sty.margin_x + c as f32 * (sty.w + sty.gap) - scroll;
    let near = ((sty.margin_x + sty.w * 2.5 + sty.gap - x) / sty.w).clamp(0.0, 1.0);
    sty.h * (sty.focus_scale - 1.0) * 0.5 * near
}

/// A non-focused cell body: the art tile + an optional resume bar. `rect` is the caller's
/// already-scaled rect; `s` scales the corner radius. (The home grid's non-focused cell, verbatim.)
pub(crate) fn draw_tile(p: Painter, art: Art, rect: Rect, s: f32, sty: &RowStyle, resume: Option<f32>) {
    let rad = sty.tile_radius(rect, s);
    // every tile carries a resting shadow + 1px perimeter stroke, both FOLDED into card()'s single
    // composite pass; a near-neighbour mid-pop (s slightly >1) lifts smoothly toward the focused
    // shadow via `f`.
    let f = ((s - 1.0) / sty.ring_denom()).clamp(0.0, 1.0);
    card(p, rect, art, rad, false, 1.0, f);
    if let Some(frac) = resume {
        resume_bar(p, rect, frac, rad);
    }
}

/// The metadata block under a FOCUSED tile: a centred title, optionally a second caption line
/// ("S1 • E8", a year, a character name), optionally led by the Continue-Watching play glyph. Only
/// the focused tile carries it, so a row builds exactly one of these per frame.
///
/// It is a type rather than three parameters because the block's height is a fact the SCREEN needs:
/// every shelf must reserve room for it in its own flow, and `reveal`/`scroll_into_view` only keep
/// air under the block a screen *declares*. Before this, five call sites hand-authored that number
/// from constants they could not see ([`RowStyle::title_lines`] even had to warn about it in a doc
/// comment) — [`TileLabel::height`] is the answer they were restating.
#[derive(Default)]
pub(crate) struct TileLabel {
    pub title: Option<std::ffi::CString>,
    pub caption: Option<std::ffi::CString>,
    /// lead the title with the amber play triangle (Continue Watching's tile has no play disc)
    pub glyph: bool,
}

impl TileLabel {
    /// Title only — the plain poster shelf.
    pub(crate) fn title(t: &str) -> Self {
        TileLabel { title: std::ffi::CString::new(t).ok(), ..Default::default() }
    }
    /// Title + a caption line; an empty caption is simply absent (never a blank row).
    pub(crate) fn titled(t: &str, caption: &str) -> Self {
        TileLabel { caption: std::ffi::CString::new(caption).ok().filter(|_| !caption.is_empty()), ..Self::title(t) }
    }
    /// Title led by the amber play triangle — Continue Watching's focused primary line.
    pub(crate) fn played(t: &str) -> Self {
        TileLabel { glyph: true, ..Self::title(t) }
    }
    /// THE height a screen must reserve under the poster row for this block, at `sty`. Derived from
    /// the same three constants [`draw_focused`] lays it out with, so a screen's declared band and
    /// the block on screen cannot drift — which is the whole reason raising
    /// [`RowStyle::title_lines`] is now safe.
    pub(crate) fn height(sty: &RowStyle, caption: bool) -> f32 {
        UNDER_DROP
            + sty.title_lines as f32 * UNDER_LINE_H
            + if caption { UNDER_LINE_GAP + UNDER_CAPTION_H } else { 0.0 }
    }
    /// [`TileLabel::height`] for a block that has whatever THIS one has.
    fn own_height(&self, sty: &RowStyle) -> f32 {
        Self::height(sty, self.caption.is_some())
    }
}

/// The focused cell body — the caller draws this LAST for its z-order: the art tile, the big glow
/// ring, an optional resume bar, then its [`TileLabel`]. (The home grid's focused-last cell.)
pub(crate) fn draw_focused(
    p: Painter,
    art: Art,
    rect: Rect,
    s: f32,
    sty: &RowStyle,
    resume: Option<f32>,
    label: &TileLabel,
) {
    let rad = sty.tile_radius(rect, s);
    // Home Screen focus treatment: soft drop-shadow + 1px perimeter sheen, both FOLDED into card()'s
    // single composite pass and ramped in with the pop `f` (the same 0→1 pop scalar the ring used).
    let f = ((s - 1.0) / sty.ring_denom()).clamp(0.0, 1.0);
    card(p, rect, art, rad, false, 1.0, f);
    if let Some(frac) = resume {
        resume_bar(p, rect, frac, rad);
    }
    // The label block anchors to the UNSCALED card bottom (rect is scaled about its center, so
    // base bottom = cy + (h/s)/2). In the mock the label is normal-flow BELOW the transformed
    // poster — a pop/press never moves it; the pop eats the poster→label gap instead of shoving
    // the label into the next shelf's title.
    let mut ty = rect.y + rect.h * 0.5 + (rect.h / s) * 0.5 + UNDER_DROP;
    if let Some(t) = &label.title {
        // Continue-Watching's focused primary line leads with an amber play glyph (the tile has no
        // play disc); every other shelf keeps the plain centred title, wrapped when the style asks.
        let drawn = if label.glyph {
            play_label(p, rect, sty, t.as_ptr(), ty);
            UNDER_LINE_H
        } else if sty.title_lines > 1 {
            wrapped_title(p, rect, sty, t.as_ptr(), ty)
        } else {
            under_label(p, rect, sty, t.as_ptr(), ty, theme::size::LABEL, 1, theme::TEXT_PRIMARY);
            UNDER_LINE_H
        };
        ty += drawn + UNDER_LINE_GAP;
    }
    if let Some(c) = &label.caption {
        under_label(p, rect, sty, c.as_ptr(), ty, theme::size::CAPTION, 0, theme::TEXT_SECONDARY);
    }
}

/// THE one-row strip loop: draw a whole horizontal shelf, non-focused tiles first (off-axis
/// culled) and the focused tile LAST for its z-order — the in-row focused-last rule, which is all
/// a lone strip needs (home's `Grid` keeps its own CROSS-row pass, which this deliberately does
/// not try to own).
///
/// This is the loop the detail page's Related and Cast rows and the person page's Movies/Shows
/// shelves all run; it lives here rather than in a screen so the culling discipline travels with
/// it. **Only tiles that pass `on_axis` are drawn, and therefore only they call `resolve_tex`** —
/// the 64-slot poster LRU must never see an off-screen request, which is exactly what a
/// hand-rolled per-screen copy of this loop keeps getting wrong.
///
/// The caller supplies the per-index content as closures: `art` the tile's [`Art`], `resume` its
/// amber progress fraction (`None` = not in progress), `label` the FOCUSED tile's [`TileLabel`]
/// (`TileLabel::default()` for a row that captions every tile through `extra` instead), and `extra`
/// any per-tile caption drawn for EVERY tile (cast names/roles). `label` is invoked for the focused
/// index only. `axis_span` widens the cull band where captions should survive slightly off-screen.
#[allow(clippy::too_many_arguments)]
pub(crate) fn strip<'a>(
    p: Painter,
    row: &CardRow,
    n: usize,
    focus_col: std::os::raw::c_int,
    row_y: f32,
    size: (f32, f32),
    pitch: f32,
    sty: &RowStyle,
    axis_span: f32,
    art: impl Fn(usize) -> Art<'a>,
    resume: impl Fn(usize) -> Option<f32>,
    label: impl Fn(usize) -> TileLabel,
    extra: impl Fn(Painter, usize, f32, bool),
) {
    let sx = row.scroll_x();
    let pr = p.translate(-sx, 0.0);
    for i in 0..n {
        if i as std::os::raw::c_int == focus_col {
            continue; // focused tile drawn last
        }
        let x = sty.margin_x + i as f32 * pitch;
        if !crate::ui::on_axis(x - sx, size.0, axis_span, 0.0) {
            continue;
        }
        let s = row.scale(i);
        let rect = Rect::new(x, row_y, size.0, size.1).scaled(s);
        draw_tile(pr, art(i), rect, s, sty, resume(i));
        extra(pr, i, x, false);
    }
    if focus_col >= 0 && (focus_col as usize) < n {
        let i = focus_col as usize;
        let x = sty.margin_x + i as f32 * pitch;
        // fold the ui::press click dip into the focused tile's scale (1.0 when idle) — same as home
        let s = row.scale(i) * crate::ui::press::scale();
        let rect = Rect::new(x, row_y, size.0, size.1).scaled(s);
        draw_focused(pr, art(i), rect, s, sty, resume(i), &label(i));
        extra(pr, i, x, true);
    }
}

// The under-tile metadata block's four metrics — the ONLY authority on its geometry, which
// [`TileLabel::height`] hands back to the screens that must reserve room for it.
/// poster bottom → the title block's first line.
const UNDER_DROP: f32 = 30.0;
/// one title line's pitch (`Person Screen v2.dc.html`'s `line-height: 30px`).
const UNDER_LINE_H: f32 = 30.0;
/// title block → caption (the mock's `margin-top: 4px`).
const UNDER_LINE_GAP: f32 = 4.0;
/// the caption's own line box. `UNDER_DROP + UNDER_LINE_H + UNDER_LINE_GAP + this` = 92, which is
/// exactly the band `detail.rs` hand-authored for its one-line-title-plus-role cast tiles — the
/// coincidence that confirms the decomposition.
const UNDER_CAPTION_H: f32 = 28.0;

/// The focused tile's title WORD-WRAPPED to `sty.title_lines`, centred under the tile — for rows
/// whose items have real titles rather than short ones ([`RowStyle::title_lines`]). Returns the
/// lines actually drawn, so the caller advances the caption by the block's true height instead of
/// leaving a hole under a one-line title.
///
/// Built on the shared [`TextView`](crate::ui::text_view::TextView) (pixel wrap + its own ellipsis
/// + a wrap cache), NOT a second hand-rolled wrapper. Its frame is the tile-plus-gaps budget
/// [`under_label`] uses, clamped to the screen so an edge tile's block cannot run off the panel.
fn wrapped_title(p: Painter, rect: Rect, sty: &RowStyle, text: *const c_char, y: f32) -> f32 {
    let (sz, bold) = (theme::size::LABEL, 1);
    let budget = under_budget(sty);
    let s = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
    let x = (rect.cx() - budget * 0.5).clamp(EDGE_PAD, SCR_W - budget - EDGE_PAD);
    // `y` is [`under_label`]'s raw glyph-texture top (it hands it straight to `Painter::text`),
    // while `TextView` positions line 0 by its CAP BAND — so the one-line and wrapped blocks only
    // start on the same row if the cap offset is added back here.
    let (cap_top, _) = crate::text::text_cap_band(sz, bold);
    // the DRAWN height, not a second measure: one wrap, so the caption's offset and the block on
    // screen cannot disagree about how many lines there were
    crate::ui::text_view::TextView::new(&s, sz, theme::TEXT_PRIMARY)
        .bold()
        .h(crate::ui::label::HAlign::Center)
        .leading(UNDER_LINE_H)
        .max_lines(sty.title_lines)
        .draw(p, Rect::new(x, y + cap_top, budget, 0.0))
        .max(UNDER_LINE_H)
}

/// The under-tile text budget: the tile plus one gap either side, from the style's UNSCALED width.
///
/// **Unscaled is load-bearing, not tidiness.** `rect` is the focus-popped tile, so a budget taken
/// from it sweeps ~310→332px across a pop — which re-keys `TextView`'s and `elide`'s width-keyed
/// caches on *every frame of the animation*, and both clear wholesale at 512 entries, so one walk
/// of a 24-tile shelf evicts every other screen's wrapped text. It also reflowed the title's line
/// breaks mid-pop. The block's y anchor is already unscaled two lines up, for the same reason.
#[inline]
fn under_budget(sty: &RowStyle) -> f32 {
    sty.w + 2.0 * sty.gap
}
/// Air kept between an edge tile's label block and the panel edge.
const EDGE_PAD: f32 = 16.0;

/// One centered metadata line under a focused tile: elided to the tile-plus-gaps budget and kept
/// inside the screen edges — a long episode title under an edge tile used to run off the panel.
fn under_label(
    p: Painter,
    rect: Rect,
    sty: &RowStyle,
    text: *const c_char,
    y: f32,
    sz: std::os::raw::c_int,
    bold: std::os::raw::c_int,
    col: [f32; 4],
) {
    let budget = under_budget(sty);
    let s = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
    let short = crate::text::elide(&s, budget, sz, bold, false);
    if let Ok(tc) = std::ffi::CString::new(short) {
        let half = crate::text::text_width(tc.as_ptr(), sz, bold) * 0.5;
        let cx = rect.cx().clamp(half + EDGE_PAD, SCR_W - half - EDGE_PAD);
        p.text(tc.as_ptr(), cx, y, sz, col, 1, bold);
    }
}

/// The focused Continue-Watching card's primary line: an amber play triangle followed by the
/// episode/movie name, the [icon + gap + name] group centred under the tile (Home Screen.dc — the
/// play affordance lives here, not as a disc on the poster). Left-aligns the name after the glyph
/// and keeps the whole group inside the screen edges.
fn play_label(p: Painter, rect: Rect, sty: &RowStyle, text: *const c_char, y: f32) {
    let (sz, bold) = (theme::size::LABEL, 1);
    let isz = (sz as f32 * 0.72).round();
    let gap = 10.0f32;
    let budget = under_budget(sty) - (isz + gap);
    let s = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
    let short = crate::text::elide(&s, budget, sz, bold, false);
    let Ok(tc) = std::ffi::CString::new(short) else { return };
    let tw = crate::text::text_width(tc.as_ptr(), sz, bold);
    let gw = isz + gap + tw;
    let cx = rect.cx().clamp(gw * 0.5 + EDGE_PAD, SCR_W - gw * 0.5 - EDGE_PAD);
    let gl = cx - gw * 0.5;
    let (ct, cb) = crate::text::text_cap_band(sz, bold);
    let icy = y + (ct + cb) * 0.5; // centre the glyph on the name's cap band
    crate::ui::icons::draw(
        p,
        crate::ui::icons::Icon::Play,
        Rect::new(gl, icy - isz * 0.5, isz, isz),
        theme::RESUME_FILL,
    );
    p.text(tc.as_ptr(), gl + isz + gap, y, sz, theme::TEXT_PRIMARY, 0, bold);
}

/// Full-bleed resume bar: the bottom band of the card itself (Continue Watching). Delegates to
/// [`crate::ui::widgets::progress_bar`], which is the ONE bar the whole app draws — a poster shelf and
/// the detail page's episode filmstrip now make the identical call, and its doc carries the two
/// pixel-level rules (snapped band, disjoint track/fill) that a hand-rolled copy here kept getting
/// subtly wrong. Only the HEIGHT is local, because it is poster-relative.
fn resume_bar(p: Painter, r: Rect, frac: f32, rad: f32) {
    // 5px on a resting poster (and it rides the focus pop): the band's bottom ~1.5px are the card
    // edge's own SDF AA ramp, so a 4px band reads ~2.5px — 5px leaves the mock's ~4px of solid color.
    let bh = (r.h * 5.0 / 375.0).max(4.0);
    crate::ui::widgets::progress_bar(p, r, rad, bh, frac);
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;
    /// The full clearance a HOME shelf's heading needs over a popped tile.
    const FULL: f32 = RowStyle::HOME.h * (RowStyle::HOME.focus_scale - 1.0) * 0.5;

    fn run(row: &mut CardRow, frames: usize, focused: Option<usize>, sty: &RowStyle) {
        for _ in 0..frames {
            row.update(8, focused, sty, DT);
        }
    }

    /// Regression, two defects in one: the heading clearance used to be `live_scale - 1`, i.e. a
    /// readout of the focus-pop spring. So (a) the frame focus moved, the incoming cell's spring
    /// was still at rest and the heading dropped its ENTIRE lift in one frame before springing
    /// back, and (b) between those it rode the pop up and down. It is a held destination now:
    /// walking slots 0 → 1 must leave the heading exactly where it is, all the way through.
    #[test]
    fn the_heading_holds_still_while_focus_walks_the_slots_beneath_it() {
        let sty = RowStyle::HOME;
        let mut row = CardRow::new();
        run(&mut row, 180, Some(0), &sty);
        let held = row.lift();
        assert!((held - FULL).abs() < 0.1, "slot 0 must hold the full clearance ({held} vs {FULL})");

        // slot 1 is also under the heading, so nothing should move — not by a pixel, not for a frame
        for f in 0..180 {
            row.update(8, Some(1), &sty, DT);
            assert!(
                (row.lift() - held).abs() < 0.1,
                "heading moved {:.2}px at frame {f} while focus stayed under it",
                row.lift() - held
            );
        }
    }

    /// And when the popped tile really does leave, the heading settles back — once, smoothly, with
    /// no frame moving it more than a spring step (the defect moved ~16px in a single frame).
    #[test]
    fn the_heading_settles_back_smoothly_once_nothing_is_under_it() {
        let sty = RowStyle::HOME;
        let mut row = CardRow::new();
        run(&mut row, 180, Some(0), &sty);
        let mut prev = row.lift();
        for _ in 0..240 {
            row.update(8, Some(5), &sty, DT); // far down the row — no clearance needed
            let l = row.lift();
            assert!((l - prev).abs() < 3.0, "heading jumped {:.1}px in one frame", l - prev);
            prev = l;
        }
        assert!(prev < 0.5, "the heading must come all the way back down (got {prev})");
    }

    /// A row that owns no focus rests flat — including a row that just LOST focus, which used to
    /// snap its heading back the same way.
    #[test]
    fn an_unfocused_row_lifts_its_heading_by_nothing() {
        let sty = RowStyle::HOME;
        let mut row = CardRow::new();
        run(&mut row, 180, Some(0), &sty);
        run(&mut row, 240, None, &sty);
        assert!(row.lift() < 0.5);
    }

    /// The clearance tapers with distance from the heading: slots 0 and 1 sit under it and get the
    /// whole thing, slot 2 is on the ramp, and a tile far down the row leaves it alone.
    #[test]
    fn the_clearance_tapers_as_the_popped_tile_moves_away_from_the_heading() {
        let sty = RowStyle::HOME;
        let at = |c: usize| heading_clearance(Some(c), &sty, 0.0);
        assert_eq!(at(0), at(1), "the first two slots share the full clearance");
        assert!((at(0) - FULL).abs() < 0.001);
        assert!(at(2) > 0.5 && at(2) < at(1), "slot 2 is on the taper ({} vs {})", at(2), at(1));
        assert_eq!(at(4), 0.0, "a tile far from the heading must not move it");
        assert_eq!(heading_clearance(None, &sty, 0.0), 0.0);
    }

    /// A row LONGER than the spring array. Every focus treatment a tile gets — the pop, the lifted
    /// shadow, the ring — is derived from `scale(i)`, and the index used to be clamped into the
    /// array, so focusing tile 30 of a 60-name cast row popped nothing at all (it read cell 23's
    /// resting spring). The shared overflow spring must pop for the focused tile, and for it alone:
    /// its neighbours read the same spring.
    #[test]
    fn a_focused_tile_past_the_spring_array_still_pops_and_its_neighbours_do_not() {
        let sty = RowStyle::CAST;
        let mut row = CardRow::new();
        let far = MAX_ROW_ITEMS + 6; // a crew credit at the end of a long cast list
        for _ in 0..180 {
            row.update(far + 2, Some(far), &sty, DT);
        }
        assert!(
            (row.scale(far) - sty.focus_scale).abs() < 0.01,
            "the focused overflow tile must reach the focus scale (got {})",
            row.scale(far)
        );
        assert_eq!(row.scale(far + 1), 1.0, "the tile after it shares the spring, not the focus");
        assert_eq!(row.scale(far - 1), 1.0, "nor the one before it");
        assert!((row.scale(MAX_ROW_ITEMS - 1) - 1.0).abs() < 0.01, "and the last in-array cell rests");

        // focus coming back inside the array releases the overflow pop
        for _ in 0..180 {
            row.update(far + 2, Some(0), &sty, DT);
        }
        assert_eq!(row.scale(far), 1.0, "focus left the tail, so no overflow tile reads the spring");
        assert!((row.scale(0) - sty.focus_scale).abs() < 0.01, "and the in-array cell pops as ever");
    }
}
