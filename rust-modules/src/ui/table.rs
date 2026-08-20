//! ui/table.rs — a reusable, animated list/table widget (Apple-TV "settings" look).
//!
//! A single selectable list built from `Section`s of `Row`s. The focused row is drawn as a
//! light rounded "pill" that SLIDES between rows (a spring on its y), rows scroll with a spring
//! when they overflow the frame, and everything is drawn through a `Painter` so the owner can
//! fade/translate the whole thing in (the open animation). It knows nothing about playback or
//! metadata: the caller builds the sections, drives selection, and reads `sel` back — so the
//! same widget serves the in-player track menu today and a settings screen later.
#![allow(dead_code)]
use crate::ui::label::{HAlign, Label};
use crate::ui::theme;
use crate::ui::{Painter, Rect, Spring};
use std::ffi::CString;

/// small trailing chip on a row (audio-description, forced, SDH, …)
pub enum Badge {
    Ad,
    Forced,
    Sdh,
    Cc,
    Text(String),
}
impl Badge {
    fn text(&self) -> &str {
        match self {
            Badge::Ad => "AD",
            Badge::Forced => "FORCED",
            Badge::Sdh => "SDH",
            Badge::Cc => "CC",
            Badge::Text(s) => s.as_str(),
        }
    }
}

pub struct Row {
    pub label: String,
    pub detail: String, // optional sub-line ("" = none)
    pub badges: Vec<Badge>,
    pub checked: bool,  // shows the leading checkmark (the ACTIVE item)
    /// A **switch** rather than a choice: `Some(on)` states itself as the word `On`/`Off` at the
    /// row's TRAILING edge — never as a mark in the leading column. A mark says where you are, a
    /// word says what is set, and no row is allowed to say both; the on/off PAIR of marks this once
    /// drew (a ring, ticked when on) is gone from the design system, assets and all.
    pub toggle: Option<bool>,
    /// Any other trailing read-out, in the same slot and the same voice as [`Row::toggle`]'s
    /// `On`/`Off` — a row whose value is a word rather than a state ("English", "Title"). Wins over
    /// `toggle` if both are somehow set.
    pub value: Option<String>,
    /// Quieten the read-out a further step (the value is context rather than the point).
    pub value_dim: bool,
    /// THE trailing accessory icon slot (an SVG asset, never a font glyph): the drill-in
    /// chevron (via the [`Row::chevron`] sugar) or e.g. the sort menu's direction chevron.
    pub ticon: Option<crate::ui::icons::Icon>,
    /// THE leading accessory icon — the SAME column the [`Row::checked`] checkmark occupies, for
    /// lists whose rows are ACTIONS rather than a picker's options (the item context menu's
    /// `[icon] [label]` rows). `checked` wins the slot when both are set: a picker's active mark is
    /// state, an action glyph is only decoration.
    pub licon: Option<crate::ui::icons::Icon>,
    pub dim: bool,      // render dimmer (unavailable / de-emphasised)
    /// A **non-selectable hairline** that groups the rows above it from the rows below (the item
    /// menu's navigation-vs-state divider). It occupies a global row index like any other row, but
    /// [`TableView::move_sel`] steps OVER it, [`TableView::hit_row`] refuses it, and
    /// [`TableView::set_sections`] never lands the selection on one — so a caller can never focus
    /// a line that does nothing. (A second `Section` cannot do this job: a headerless section adds
    /// vertical air but draws no rule, because the hairline rides the section HEADER.)
    pub sep: bool,
}
impl Row {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), detail: String::new(), badges: Vec::new(), checked: false,
               toggle: None, value: None, value_dim: false, ticon: None, licon: None, dim: false, sep: false }
    }
    /// The grouping hairline — a row that draws a rule and cannot be focused.
    pub fn separator() -> Self {
        Self { sep: true, ..Self::new("") }
    }
    pub fn checked(mut self, v: bool) -> Self { self.checked = v; self }
    /// Mark this row a SWITCH at the given state — see [`Row::toggle`].
    pub fn toggle(mut self, on: bool) -> Self { self.toggle = Some(on); self }
    /// Give this row a trailing read-out — see [`Row::value`].
    pub fn value(mut self, v: impl Into<String>) -> Self { self.value = Some(v.into()); self }
    /// Quieten the read-out a step — see [`Row::value_dim`].
    pub fn value_dim(mut self, v: bool) -> Self { self.value_dim = v; self }
    /// The word this row's trailing slot shows, if any: an explicit [`Row::value`], else a switch's
    /// `On`/`Off`. ONE resolver, so the two can never both be drawn.
    fn readout(&self) -> Option<&str> {
        match (&self.value, self.toggle) {
            (Some(v), _) => Some(v.as_str()),
            (None, Some(on)) => Some(if on { "On" } else { "Off" }),
            (None, None) => None,
        }
    }
    pub fn detail(mut self, d: impl Into<String>) -> Self { self.detail = d.into(); self }
    pub fn badge(mut self, b: Badge) -> Self { self.badges.push(b); self }
    /// sugar: the "›" drill-in affordance is just the Chevron icon in the trailing slot
    pub fn chevron(mut self, v: bool) -> Self {
        if v { self.ticon = Some(crate::ui::icons::Icon::Chevron); }
        self
    }
    pub fn ticon(mut self, i: crate::ui::icons::Icon) -> Self { self.ticon = Some(i); self }
    pub fn licon(mut self, i: crate::ui::icons::Icon) -> Self { self.licon = Some(i); self }
    pub fn dim(mut self, v: bool) -> Self { self.dim = v; self }
    fn height(&self) -> f32 {
        if self.sep { SEP_H } else if self.detail.is_empty() { ROW_H } else { ROW_H_TALL }
    }
}

pub struct Section {
    pub header: String,    // "" = no header row
    /// Right-aligned accessory on the HEADER line — a second fact about the group, one rung down
    /// and in the header's own dim ink ("Dolby Atmos"; the Sources list's owner handle beside a
    /// machine name). It is the last run on that line and is **elided** to [`ACCESSORY_W`], so a
    /// long plex.tv handle truncates on a character instead of colliding with the header or
    /// widening the panel — the rows below it are never touched.
    pub accessory: String,
    /// Dim the WHOLE group — header, accessory and every row — at one alpha.
    ///
    /// Deliberately not [`Row::dim`] applied row by row: a row's dim is an ink role, and the state
    /// this expresses (an unreachable server, still granted and still pinned) is a fact about the
    /// GROUP, whose header would otherwise stay the brightest thing in the panel. One alpha over
    /// the lot is also what keeps a dimmed row's own read-out ("On") legible as itself: nothing was
    /// unpinned, so nothing may read as off.
    pub dim: bool,
    pub rows: Vec<Row>,
}
impl Section {
    pub fn new(header: impl Into<String>) -> Self {
        Self { header: header.into(), accessory: String::new(), dim: false, rows: Vec::new() }
    }
    pub fn accessory(mut self, a: impl Into<String>) -> Self { self.accessory = a.into(); self }
    /// Dim the whole group at [`GROUP_DIM_A`] — see [`Section::dim`].
    pub fn dim(mut self, v: bool) -> Self { self.dim = v; self }
    pub fn row(mut self, r: Row) -> Self { self.rows.push(r); self }
}

/// A [`Row::separator`]'s row height. The hairline sits on its centre line, so this IS the gap
/// between the two groups it divides — a gap between stacked blocks comes from a `space` rung, so
/// it is the rung, not a hand-tuned number.
const SEP_H: f32 = theme::space::MD;
/// The list's own vertical padding: the amount an owning panel must subtract from its height to get
/// the visible list height for [`TableView::update`]. Exposed because each popover screen was
/// re-hardcoding `panel_h - 40.0` and would drift the moment either pad changed.
pub const PAD_V: f32 = TOP_PAD + BOT_PAD;
/// A plain row (label only) — mockup rowBase padding 13 + 34px label.
///
/// `pub` for the same caller shape [`CONTENT_X`] is: a block that draws ROWS of its own on the
/// app's ground rather than mounting a [`TableView`] (`ui/search/recents.rs` — its rows are the
/// user's own words and have to stay editable in place). It re-derived this and the four constants
/// below from the mockup, so a row-height change here silently misaligned that block while both
/// modules' own tests stayed green.
pub const ROW_H: f32 = 60.0;
const ROW_H_TALL: f32 = 92.0; // a row that carries a detail sub-line (title HEADLINE + detail CAPTION)
const ROW_SUB_GAP: f32 = 15.0; // title baseline → detail cap-top, in a two-line row
/// Panel header ("AUDIO"/"SUBTITLES", a server over its libraries). 58px in BOTH size classes and
/// whatever the header's own size — the band is fixed so a size change cannot reflow a panel.
/// `pub` for the [`ROW_H`] caller shape.
pub const HDR_H: f32 = 58.0;
/// Measure a [`Section::accessory`] is elided to. It is the LAST run on the header line, so it is
/// the one that gives way: a 34-character plex.tv handle truncates on a character and the library
/// names underneath keep their full width.
const ACCESSORY_W: f32 = 320.0;
/// The alpha a [`Section::dim`]med group is drawn at — the same weight an unreachable tab pill
/// takes, applied once over header and rows together.
const GROUP_DIM_A: f32 = 0.52;
const DIV_H: f32 = 24.0; // gap + hairline between sections
/// The list's own air above its first row. `pub` because a panel that stacks something ABOVE the
/// list (the Sources panel's level band) has to subtract it to put the SEAM on the space scale —
/// otherwise the two paddings add and the gap lands between rungs.
pub const TOP_PAD: f32 = 20.0;
const BOT_PAD: f32 = 20.0;
/// Pill (row) inset from the panel's left/right. `pub` for the [`ROW_H`] caller shape.
pub const SIDE: f32 = 12.0;
const CONTENT_PAD: f32 = 20.0; // text/check padding inside the pill (mockup rowBase 13px 20px)
/// Where a row's own content starts, measured from the panel's left edge. Exposed so a panel that
/// draws chrome ABOVE the list (the Sources panel's level pills) can start it on the same line the
/// rows do, instead of re-deriving two private constants and drifting from them.
pub const CONTENT_X: f32 = SIDE + CONTENT_PAD;
const CHECK_W: f32 = 32.0; // leading check column
const GAP: f32 = 16.0; // check→label gap
/// Focused-row pill corner radius. `pub` for the [`ROW_H`] caller shape.
pub const PILL_RAD: f32 = 18.0;
/// Pill inset from the row's top/bottom. `pub` for the [`ROW_H`] caller shape.
pub const PILL_INSET: f32 = 3.0;
const PANEL_BG: [f32; 4] = theme::SURFACE_PANEL; // opaque panel colour — fade masks + badge knockout
/// Air between two chips of one right-aligned badge run (a subtitle row's `FORCED` + `SDH`).
const BADGE_GAP: f32 = 10.0;
/// The trailing read-out's WEIGHT: `size::LABEL` **bold**, which is what the `PlxNative Design
/// System`'s `TableView` authors it as (`var(--font-weight-bold) var(--size-label)`) and what its
/// prose says in words. The product drew it regular until 2026-08-21 — a rung below the row's
/// HEADLINE label in size, a step behind it in ink, AND a weight lighter, which is three
/// de-emphases stacked on the one run that answers the row's question. The read-out is not a
/// caption of the label; it is the value, and it has to hold its own beside the badge run next to
/// it. The step back is the INK's alone.
///
/// ONE constant, because the three calls at the draw site have to agree: the cap band the run is
/// centred on, the advance width it adds to `trailing`, and the paint. Bold glyphs are WIDER than
/// their regular twins, so measuring on one flag and painting on the other under-counts `trailing`
/// — and `trailing` is the LABEL's elision budget (`text_w` below), so the label would be elided
/// as though the read-out were narrower than it is and run right into it. The BADGE run is not at
/// risk and cannot be: it is placed before this block adds `vw`, which is exactly why the read-out
/// sits INSIDE it rather than the other way round.
const VALUE_BOLD: std::os::raw::c_int = 1;

pub struct TableView {
    pub sections: Vec<Section>,
    pub sel: i32,  // global index across all sections' rows (headers are not selectable)
    /// Does the LIST hold focus? `false` draws no selection pill at all — the "nothing selected"
    /// mode this widget did not have.
    ///
    /// It exists for a panel with a control OUTSIDE the list (the Sources panel's Browse / On Home
    /// segments): while focus is up there, a table that always paints its selection puts a second
    /// accent capsule on screen, and the two are indistinguishable. `sel` is REMEMBERED across the
    /// trip — you come back to the row you left, so this suppresses the pill rather than clearing
    /// the selection.
    ///
    /// Defaults to `true`, so every panel whose list is the only focusable thing is unchanged.
    pub list_focused: bool,
    /// compact size class: BODY regular row LABELS instead of the default HEADLINE bold (the small
    /// account popover; HEADLINE-bold rows overwhelmed a 440px panel of one-word actions). Set it
    /// on every ACTION menu — the item context menu, account, more, and the library sort/filter/
    /// genre menus all do; only a PICKER of title+detail rows stays on the default.
    ///
    /// It no longer affects HEADERS: those are CAPS at CAPTION in both classes, because the caps
    /// are what make a header a label and a size that varied could tie with its own rows.
    pub compact: bool,
    // the highlight pill's top and bottom edges spring INDEPENDENTLY (content coords), so moving
    // to a taller/shorter row morphs the pill smoothly instead of snapping its height.
    hl_top: Spring,
    hl_bot: Spring,
    scroll: Spring,
}
impl TableView {
    pub const fn new() -> Self {
        Self {
            sections: Vec::new(),
            sel: 0,
            list_focused: true,
            compact: false,
            hl_top: Spring::at(0.0),
            hl_bot: Spring::at(0.0),
            scroll: Spring::at(0.0),
        }
    }

    /// The row under the pointer in a `frame`-anchored draw (screen coords), or None — popover
    /// click support (hover→focus, click→commit) shares the draw's own layout walk.
    pub fn hit_row(&self, frame: Rect, mx: f32, my: f32) -> Option<i32> {
        if !frame.contains(mx, my) {
            return None;
        }
        let top0 = frame.y + TOP_PAD;
        let scroll = self.scroll.pos;
        let mut hit = None;
        self.walk(|cy, gi, _| {
            if gi < 0 || self.rows_at(gi).sep {
                return; // headers and grouping hairlines are not click targets
            }
            let sy = top0 + cy - scroll;
            if my >= sy && my <= sy + self.rows_at(gi).height() {
                hit = Some(gi);
            }
        });
        hit
    }

    /// replace the contents and re-anchor selection. `slide=false` snaps the pill to the new
    /// selection (use when the whole list changed, e.g. Audio↔Subtitles); `true` lets it glide.
    pub fn set_sections(&mut self, sections: Vec<Section>, sel: i32, slide: bool) {
        self.sections = sections;
        let n = self.n_rows();
        self.sel = if n == 0 { 0 } else { self.settle_sel(sel) };
        if !slide {
            let top = self.row_top(self.sel);
            self.hl_top.jump(top + PILL_INSET);
            self.hl_bot.jump(top + self.row_height(self.sel) - PILL_INSET);
            self.scroll.jump(0.0);
        }
    }

    pub fn n_rows(&self) -> i32 {
        self.sections.iter().map(|s| s.rows.len()).sum::<usize>() as i32
    }

    /// the full drawn height of the content (headers + rows + top/bottom padding) — the owner
    /// sizes its panel to this (clamped) so the panel hugs the list, tvOS-style.
    pub fn measured_height(&self) -> f32 {
        if self.n_rows() == 0 {
            return 120.0;
        }
        self.content_h() + TOP_PAD + BOT_PAD
    }

    /// The nearest **selectable** row to `i`: `i` itself when it is one, else the first non-separator
    /// after it, else the last one before it — so selection cannot come to rest on a grouping
    /// hairline, whoever set it. The one exception is a list that is ALL separators, which has no
    /// selectable row to offer and gets `i` back; that is degenerate rather than defended against
    /// (nothing can commit — `on_ok` reads no action off a hairline — and nothing panics).
    fn settle_sel(&self, i: i32) -> i32 {
        let n = self.n_rows();
        if n == 0 {
            return 0;
        }
        let i = i.clamp(0, n - 1);
        if !self.rows_at(i).sep {
            return i;
        }
        (i + 1..n)
            .find(|&j| !self.rows_at(j).sep)
            .or_else(|| (0..i).rev().find(|&j| !self.rows_at(j).sep))
            .unwrap_or(i)
    }

    /// Move the selection `delta` **selectable** rows: a separator is skipped rather than landed on,
    /// so one DOWN across the item menu's divider lands on the first state action, not on the rule.
    /// A step that would run off either end is dropped (the selection stays put), matching the old
    /// clamp.
    pub fn move_sel(&mut self, delta: i32) {
        let n = self.n_rows();
        if n == 0 {
            return;
        }
        let step = if delta < 0 { -1 } else { 1 };
        let mut cur = self.settle_sel(self.sel);
        for _ in 0..delta.abs() {
            let mut j = cur + step;
            while j >= 0 && j < n && self.rows_at(j).sep {
                j += step;
            }
            if j < 0 || j >= n {
                break;
            }
            cur = j;
        }
        self.sel = cur;
    }

    /// walk the visual layout top-to-bottom, invoking `f(content_y, global_row_index_or_-1, sec)`
    /// once per header (gi = -1) and once per row (gi >= 0). Cheap; the lists are short.
    fn walk(&self, mut f: impl FnMut(f32, i32, usize)) {
        let mut y = 0.0f32;
        let mut gi = 0i32;
        for (si, sec) in self.sections.iter().enumerate() {
            if si > 0 {
                y += DIV_H;
            }
            if !sec.header.is_empty() {
                f(y, -1, si);
                y += HDR_H;
            }
            for row in &sec.rows {
                f(y, gi, si);
                y += row.height();
                gi += 1;
            }
        }
    }

    fn row_top(&self, target: i32) -> f32 {
        let mut out = 0.0;
        self.walk(|y, gi, _| {
            if gi == target {
                out = y;
            }
        });
        out
    }
    fn row_height(&self, target: i32) -> f32 {
        let mut n = 0i32;
        for sec in &self.sections {
            for row in &sec.rows {
                if n == target {
                    return row.height();
                }
                n += 1;
            }
        }
        ROW_H
    }
    fn content_h(&self) -> f32 {
        let mut h = 0.0;
        self.walk(|y, _, _| h = y); // last item's top
        // add the last row's height
        h + self.row_height(self.n_rows() - 1)
    }

    pub fn update(&mut self, dt: f32, visible_h: f32) {
        if self.n_rows() == 0 {
            return;
        }
        let top = self.row_top(self.sel);
        let rh = self.row_height(self.sel);
        // top and bottom edges spring independently → the pill stretches/morphs between rows
        self.hl_top.step(top + PILL_INSET, 360.0, dt);
        self.hl_bot.step(top + rh - PILL_INSET, 360.0, dt);
        // keep the selection (plus a row of context) inside the viewport
        let content_h = self.content_h();
        let max_scroll = (content_h - visible_h).max(0.0);
        let mut sc = self.scroll.pos;
        if top - rh < sc {
            sc = top - rh;
        }
        if top + 2.0 * rh > sc + visible_h {
            sc = top + 2.0 * rh - visible_h;
        }
        sc = sc.clamp(0.0, max_scroll);
        self.scroll.step(sc, 300.0, dt);
    }

    pub fn draw(&self, p: Painter, frame: Rect) {
        if self.n_rows() == 0 {
            Label::new(c"No tracks".as_ptr(), theme::size::BODY, theme::TEXT_TERTIARY).h(HAlign::Center).draw(p, frame);
            return;
        }
        // Hard-clip everything below to the panel frame: the list overflows, so a partial edge row is
        // cut cleanly at the frame instead of poking over the video / control buttons — and, unlike the
        // old fade masks, a tall two-line edge row is cut uniformly (the fade left its title bright but
        // faded its detail line, which read as a broken clip). Released at the end of this fn.
        p.clip(frame);
        let top0 = frame.y + TOP_PAD;
        let scroll = self.scroll.pos;
        let vis_top = frame.y;
        let vis_bot = frame.y + frame.h;

        let white = theme::TEXT_PRIMARY;
        let dimc = theme::TEXT_TERTIARY; // was #8a8a8e; unified onto the tertiary grey
        let ink = crate::ui::ACCENT_INK; // text/glyph over the light pill
        let content_x = frame.x + SIDE + CONTENT_PAD;
        let label_x = content_x + CHECK_W + GAP;
        let text_right = frame.x + frame.w - SIDE - CONTENT_PAD;

        // ---- sliding pill (under the rows) — warm off-white; top/bottom edges morph independently ----
        // It follows its GROUP's dim: the focused row of an unreachable server must not be the one
        // bright thing in a panel that is telling you the server is unreachable.
        let py0 = top0 + self.hl_top.pos - scroll;
        let py1 = top0 + self.hl_bot.pos - scroll;
        let pill = Rect::new(frame.x + SIDE, py0, frame.w - 2.0 * SIDE, (py1 - py0).max(1.0));
        if self.list_focused && pill.y + pill.h > vis_top && pill.y < vis_bot {
            self.group_painter(p, self.section_of_row(self.sel)).rrect(pill, PILL_RAD, PILL_RAD, crate::ui::ACCENT);
        }

        // ---- headers + rows ----
        self.walk(|cy, gi, si| {
            // ONE alpha over the whole group (see `Section::dim`) — pushed here, at the top of the
            // walk, so header, accessory, rows, marks and read-outs can never dim out of step.
            let p = self.group_painter(p, si);
            let sy = top0 + cy - scroll;
            if gi == -1 {
                // panel/section header (+ hairline divider above later sections); scissor-clipped to `frame`
                if sy + HDR_H > vis_top && sy < vis_bot {
                    let sec = &self.sections[si];
                    if si > 0 {
                        p.rect(Rect::new(content_x, sy - DIV_H * 0.5, frame.w - 2.0 * (SIDE + CONTENT_PAD), 2.0),
                            0.0, theme::HAIRLINE, theme::HAIRLINE, 0.0);
                    }
                    // **CAPS at CAPTION, one size in BOTH size classes.** The caps are what make a
                    // header read as a label rather than as a row, which is why the size stops
                    // varying: at HEADLINE — what the non-compact class used to draw — a header
                    // ties with or outweighs the rows it heads, and on the Sources panel (a
                    // two-line picker, so non-compact) that put the machine names on screen bigger
                    // than the libraries they name. Design system, `TableView.prompt.md`.
                    //
                    // `to_uppercase`, not `to_ascii_uppercase`: a header is a machine name or a
                    // library name and can be any script — the share measured here is Cyrillic.
                    let hsz = theme::size::CAPTION;
                    if let Ok(cs) = CString::new(sec.header.to_uppercase()) {
                        p.text(cs.as_ptr(), content_x, sy + 8.0, hsz, dimc, 0, 0);
                    }
                    // trailing accessory, one rung down and on the header's own baseline. Elided,
                    // because it is the run that gives way (see `Section::accessory`).
                    if !sec.accessory.is_empty() {
                        // MICRO, a rung below the header: "the header names the group, the
                        // accessory only qualifies it, so it never ties with the header"
                        // (`TableView.prompt.md`). At CAPTION the two were the same size and a
                        // plex.tv handle read as loud as the machine it hangs off.
                        let asz = theme::size::MICRO;
                        let a = crate::text::elide(&sec.accessory, ACCESSORY_W, asz, 0, false);
                        if let Ok(ac) = CString::new(a) {
                            let ay = crate::text::baseline_y(asz, 0, hsz, 0, sy + 8.0);
                            p.text(ac.as_ptr(), text_right, ay, asz, dimc, 2, 0);
                        }
                    }
                }
                return;
            }
            let row = self.rows_at(gi);
            let h = row.height();
            if sy + h < vis_top || sy > vis_bot {
                return; // fully scrolled out; a partial edge row is drawn and scissor-clipped to `frame`
            }
            if row.sep {
                // grouping hairline, on the row's centre line and inset to the label column so it
                // reads as a divider between groups rather than a full-bleed panel rule
                p.rect(Rect::new(content_x, sy + h * 0.5 - 1.0, frame.w - 2.0 * (SIDE + CONTENT_PAD), 2.0),
                    0.0, theme::HAIRLINE, theme::HAIRLINE, 0.0);
                return;
            }
            // **`list_focused` gates the INK, not just the pill.** `ink` is near-black and is only
            // legible ON the accent pill; suppressing the pill while still flipping the ink drew
            // black-on-panel rows, which is what the owner saw the moment focus moved to the
            // Sources panel's level pills. The two are one decision and are read from one flag.
            let focused = gi == self.sel && self.list_focused;
            let base = if focused { ink } else if row.dim { dimc } else { white };
            let row_bg = if focused { crate::ui::ACCENT } else { PANEL_BG };
            let cyc = sy + h * 0.5; // row vertical center

            // Leading column (SVG): the PICKER's tick, or an ACTION's glyph — one or the other, and
            // never a switch. A mark here says WHERE YOU ARE; what a row is SET to is a word at the
            // trailing edge (below), and no row is allowed to say both.
            let lead = if row.checked { Some(crate::ui::icons::Icon::Check) } else { row.licon };
            if let Some(li) = lead {
                let cs = 26.0f32;
                let cr = Rect::new(content_x + (CHECK_W - cs) * 0.5, cyc - cs * 0.5, cs, cs);
                crate::ui::icons::draw(p, li, cr, base);
            }
            // trailing accessory (SVG)
            let mut trailing = 0.0f32;
            if let Some(ti) = row.ticon {
                let cs = 26.0f32;
                let cr = Rect::new(text_right - cs, cyc - cs * 0.5, cs, cs);
                crate::ui::icons::draw(p, ti, cr, base);
                trailing = cs + 14.0;
            }
            // PLACE 4 — the badge run, RIGHT-ALIGNED at the trailing edge and the outermost of the
            // three trailing runs (the design system's cell is a flex row whose label block takes
            // all the slack, so `badge` — written last — is flush right, with the read-out beside
            // it). It was drawn INLINE, starting at the label's own drawn right edge, which gave a
            // chip the design pins flush a per-ROW x: a column of resolution classes, or of
            // FORCED/SDH/codec tags, landed wherever each label happened to end, so the panel read
            // as ragged text rather than as a column you can compare straight down.
            //
            // Centred on the ROW BOX (`cyc`), not on the title's cap band: the chip is a sibling of
            // the whole label block, so on a two-line row it sits level with the pair rather than
            // ~16px high, level with the first line.
            if !row.badges.is_empty() {
                let run: f32 = row.badges.iter().map(|b| crate::ui::widgets::badge_w(b.text(), None)).sum::<f32>()
                    + BADGE_GAP * (row.badges.len() - 1) as f32;
                let mut bx = text_right - trailing - run;
                for b in row.badges.iter() {
                    // the shared chip leaf in this row's contextual colours: the row's own ink for
                    // the label, the design's keyline for the ring (the pill's near-black ink over
                    // a focused row, where a 55%-white stroke would vanish), the row's ground
                    // knocked out of the interior
                    let sty = crate::ui::widgets::BadgeStyle::Outlined {
                        col: base,
                        border: if focused { base } else { theme::OVERLAY_BORDER },
                        bg: row_bg,
                    };
                    bx += crate::ui::widgets::badge(p, bx, cyc, b.text(), None, sty) + BADGE_GAP;
                }
                trailing += run + 14.0;
            }
            // Trailing VALUE — the read-out that says what this row is set to ("On"/"Off" for a
            // switch, or any word). One step behind the label in ink, so the label is what you read
            // and the value is what you check; over the focused row's near-white pill that step is
            // an alpha of the pill's own ink rather than a grey, which would go muddy on it. That
            // step is the ink's ALONE — the run itself is bold (see [`VALUE_BOLD`], which all three
            // calls below take so the measure and the paint can never be two different faces).
            if let Some(v) = row.readout() {
                let ink = match (focused, row.value_dim) {
                    (true, false) => theme::ROW_VALUE_INK_ON,
                    (true, true) => theme::ROW_VALUE_INK_ON_DIM,
                    (false, false) => theme::TEXT_SECONDARY,
                    (false, true) => theme::TEXT_TERTIARY,
                };
                if let Ok(vc) = std::ffi::CString::new(v) {
                    let vsz = theme::size::LABEL;
                    let vy = crate::text::text_vcenter_y(vsz, VALUE_BOLD, cyc);
                    let vw = crate::text::text_width(vc.as_ptr(), vsz, VALUE_BOLD);
                    p.text(vc.as_ptr(), text_right - trailing, vy, vsz, ink, 2, VALUE_BOLD);
                    trailing += vw + 14.0;
                }
            }
            // Single-line rows centre their label on the row by cap band. Two-line rows stack a
            // title over a detail sub-line: lay the pair out off both cap bands and centre it in the
            // tall row, with an explicit gap between the title baseline and the detail cap-top
            // (layout ≠ paint — the old fixed offsets left the two lines cramped after centring).
            let two_line = !row.detail.is_empty();
            // two-line rows follow the table's size class too (compact = BODY regular titles)
            let (tsz, tbold) = if self.compact { (theme::size::BODY, 0) } else { (theme::size::HEADLINE, 1) };
            let (title_y, detail_y) = if two_line {
                let (t_top, t_base) = crate::text::text_cap_band(tsz, tbold);
                let (d_top, d_base) = crate::text::text_cap_band(theme::size::CAPTION, 0);
                let (t_cap, d_cap) = (t_base - t_top, d_base - d_top); // cap heights
                let pair_gap = ROW_SUB_GAP; // title baseline → detail cap-top
                let pair_top = sy + (h - (t_cap + pair_gap + d_cap)) * 0.5; // title cap-top
                (pair_top - t_top, pair_top + t_cap + pair_gap - d_top)
            } else {
                (0.0, 0.0) // unused single-line; Label centres the label
            };
            // PLACE 2 — the label over its optional sub-line. Compact tables read their single-line
            // labels at BODY regular.
            let (lsz, lbold) = if self.compact { (theme::size::BODY, 0) } else { (theme::size::HEADLINE, 1) };
            let text_w = text_right - label_x - trailing;
            let lbl = crate::text::elide(&row.label, text_w, lsz, lbold, false);
            if let Ok(cs) = CString::new(lbl) {
                if two_line {
                    p.text(cs.as_ptr(), label_x, title_y, tsz, base, 0, tbold);
                } else {
                    let mut lab = Label::new(cs.as_ptr(), lsz, base);
                    if lbold == 1 {
                        lab = lab.bold();
                    }
                    lab.draw(p, Rect::new(label_x, sy, 0.0, h));
                }
            }
            // detail sub-line, elided (long Cyrillic descriptors would run off the edge) — to the
            // SAME right edge as the label above it, because the design system puts the pair in one
            // `flex:1` box. It used to elide against the bare row width, which was harmless only
            // while the badges sat on the title's line: right-aligned and row-centred, a chip now
            // occupies the sub-line's band too, and an unbounded sub-line would run under it.
            if !row.detail.is_empty() {
                let sub = if focused { theme::scrim_black(0.6) } else { dimc };
                let detail = crate::text::elide(&row.detail, text_w, theme::size::CAPTION, 0, false);
                if let Ok(cd) = CString::new(detail) {
                    p.text(cd.as_ptr(), label_x, detail_y, theme::size::CAPTION, sub, 0, 0);
                }
            }
        });

        p.clip_clear();
    }

    /// `p`, dimmed if section `si` is — the ONE place [`Section::dim`] becomes an alpha.
    fn group_painter(&self, p: Painter, si: usize) -> Painter {
        if self.sections.get(si).is_some_and(|s| s.dim) {
            p.alpha(GROUP_DIM_A)
        } else {
            p
        }
    }

    /// Which section global row `gi` belongs to (0 when it belongs to none — an empty table).
    fn section_of_row(&self, gi: i32) -> usize {
        let mut n = 0i32;
        for (si, sec) in self.sections.iter().enumerate() {
            n += sec.rows.len() as i32;
            if gi < n {
                return si;
            }
        }
        0
    }

    fn rows_at(&self, gi: i32) -> &Row {
        let mut n = 0i32;
        for sec in &self.sections {
            for row in &sec.rows {
                if n == gi {
                    return row;
                }
                n += 1;
            }
        }
        // unreachable for a valid gi (draw early-returns when there are no rows)
        self.sections.iter().flat_map(|s| s.rows.iter()).next().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ONE trailing read-out per row, resolved in ONE place. The rule the design system states and
    /// this widget enforces is that **a mark says where you are and a word says what is set, and no
    /// row is allowed to say both** — so a switch's state is the WORD `On`/`Off` at the trailing
    /// edge (there is no ring/ticked-ring pair any more, assets and all), a leading tick is not a
    /// read-out at all, and a row carrying both a `value` and a `toggle` draws exactly one of them.
    ///
    /// Worth pinning even though it is four lines of `match`: `library.rs`'s Sources test spells the
    /// `On`/`Off` mapping out a second time to grade the row MODEL, and this is the assertion that
    /// the copy it grades is the copy the widget actually draws.
    #[test]
    fn a_row_states_exactly_one_trailing_read_out() {
        assert_eq!(Row::new("Unwatched only").toggle(true).readout(), Some("On"));
        assert_eq!(Row::new("Unwatched only").toggle(false).readout(), Some("Off"));
        assert_eq!(Row::new("Genre").value("All").readout(), Some("All"));
        // both set: the explicit word wins and the switch's is NOT drawn beside it
        assert_eq!(Row::new("Genre").value("Comedy").toggle(true).readout(), Some("Comedy"));
        // a leading mark is a different column with a different job — it states no value
        assert_eq!(Row::new("English").checked(true).readout(), None);
        assert_eq!(Row::new("Chapters").readout(), None);
    }
}
