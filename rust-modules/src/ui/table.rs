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
    pub chevron: bool,  // trailing "›" drill-in affordance
    pub dim: bool,      // render dimmer (unavailable / de-emphasised)
}
impl Row {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), detail: String::new(), badges: Vec::new(),
               checked: false, chevron: false, dim: false }
    }
    pub fn checked(mut self, v: bool) -> Self { self.checked = v; self }
    pub fn detail(mut self, d: impl Into<String>) -> Self { self.detail = d.into(); self }
    pub fn badge(mut self, b: Badge) -> Self { self.badges.push(b); self }
    pub fn chevron(mut self, v: bool) -> Self { self.chevron = v; self }
    pub fn dim(mut self, v: bool) -> Self { self.dim = v; self }
    fn height(&self) -> f32 { if self.detail.is_empty() { ROW_H } else { ROW_H_TALL } }
}

pub struct Section {
    pub header: String,    // "" = no header row
    pub accessory: String, // right-aligned header accessory, e.g. "Dolby Atmos"
    pub rows: Vec<Row>,
}
impl Section {
    pub fn new(header: impl Into<String>) -> Self {
        Self { header: header.into(), accessory: String::new(), rows: Vec::new() }
    }
    pub fn accessory(mut self, a: impl Into<String>) -> Self { self.accessory = a.into(); self }
    pub fn row(mut self, r: Row) -> Self { self.rows.push(r); self }
}

const ROW_H: f32 = 60.0; // a plain row (label only) — mockup rowBase padding 13 + 34px label
const ROW_H_TALL: f32 = 92.0; // a row that carries a detail sub-line (title HEADLINE + detail CAPTION)
const ROW_SUB_GAP: f32 = 15.0; // title baseline → detail cap-top, in a two-line row
const HDR_H: f32 = 58.0; // panel header ("Audio"/"Subtitles"), HEADLINE
const DIV_H: f32 = 24.0; // gap + hairline between sections
const TOP_PAD: f32 = 20.0;
const BOT_PAD: f32 = 20.0;
const SIDE: f32 = 12.0; // pill (row) inset from the panel's left/right
const CONTENT_PAD: f32 = 20.0; // text/check padding inside the pill (mockup rowBase 13px 20px)
const CHECK_W: f32 = 32.0; // leading check column
const GAP: f32 = 16.0; // check→label gap
const PILL_RAD: f32 = 18.0; // focused-row pill corner radius
const PILL_INSET: f32 = 3.0; // pill inset from the row's top/bottom
const PANEL_BG: [f32; 4] = theme::SURFACE_PANEL; // opaque panel colour — fade masks + badge knockout

pub struct TableView {
    pub sections: Vec<Section>,
    pub sel: i32,  // global index across all sections' rows (headers are not selectable)
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
            hl_top: Spring::at(0.0),
            hl_bot: Spring::at(0.0),
            scroll: Spring::at(0.0),
        }
    }

    /// replace the contents and re-anchor selection. `slide=false` snaps the pill to the new
    /// selection (use when the whole list changed, e.g. Audio↔Subtitles); `true` lets it glide.
    pub fn set_sections(&mut self, sections: Vec<Section>, sel: i32, slide: bool) {
        self.sections = sections;
        let n = self.n_rows();
        self.sel = if n == 0 { 0 } else { sel.clamp(0, n - 1) };
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

    pub fn move_sel(&mut self, delta: i32) {
        let n = self.n_rows();
        if n > 0 {
            self.sel = (self.sel + delta).clamp(0, n - 1);
        }
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
        let py0 = top0 + self.hl_top.pos - scroll;
        let py1 = top0 + self.hl_bot.pos - scroll;
        let pill = Rect::new(frame.x + SIDE, py0, frame.w - 2.0 * SIDE, (py1 - py0).max(1.0));
        if pill.y + pill.h > vis_top && pill.y < vis_bot {
            p.rrect(pill, PILL_RAD, PILL_RAD, crate::ui::ACCENT);
        }

        // ---- headers + rows ----
        self.walk(|cy, gi, si| {
            let sy = top0 + cy - scroll;
            if gi == -1 {
                // panel/section header (+ hairline divider above later sections); scissor-clipped to `frame`
                if sy + HDR_H > vis_top && sy < vis_bot {
                    let sec = &self.sections[si];
                    if si > 0 {
                        p.rect(Rect::new(content_x, sy - DIV_H * 0.5, frame.w - 2.0 * (SIDE + CONTENT_PAD), 2.0),
                            0.0, theme::HAIRLINE, theme::HAIRLINE, 0.0);
                    }
                    if let Ok(cs) = CString::new(sec.header.as_str()) {
                        p.text(cs.as_ptr(), content_x, sy + 8.0, theme::size::HEADLINE, dimc, 0, 0);
                    }
                }
                return;
            }
            let row = self.rows_at(gi);
            let h = row.height();
            if sy + h < vis_top || sy > vis_bot {
                return; // fully scrolled out; a partial edge row is drawn and scissor-clipped to `frame`
            }
            let focused = gi == self.sel;
            let base = if focused { ink } else if row.dim { dimc } else { white };
            let row_bg = if focused { crate::ui::ACCENT } else { PANEL_BG };
            let cyc = sy + h * 0.5; // row vertical center

            // leading check (SVG) on the active row
            if row.checked {
                let cs = 26.0f32;
                let cr = Rect::new(content_x + (CHECK_W - cs) * 0.5, cyc - cs * 0.5, cs, cs);
                crate::ui::icons::draw(p, crate::ui::icons::Icon::Check, cr, base);
            }
            // trailing chevron (SVG)
            let mut trailing = 0.0f32;
            if row.chevron {
                let cs = 26.0f32;
                let cr = Rect::new(text_right - cs, cyc - cs * 0.5, cs, cs);
                crate::ui::icons::draw(p, crate::ui::icons::Icon::Chevron, cr, base);
                trailing = cs + 14.0;
            }
            // reserve the inline-badge run so the label elides before it
            let badge_reserve: f32 =
                row.badges.iter().map(|b| crate::ui::widgets::badge_w(b.text()) + 10.0).sum();
            // Single-line rows centre their label on the row by cap band. Two-line rows stack a
            // title over a detail sub-line: lay the pair out off both cap bands and centre it in the
            // tall row, with an explicit gap between the title baseline and the detail cap-top
            // (layout ≠ paint — the old fixed offsets left the two lines cramped after centring).
            let two_line = !row.detail.is_empty();
            let (title_y, detail_y, bcy) = if two_line {
                let (t_top, t_base) = crate::text::text_cap_band(theme::size::HEADLINE, 1);
                let (d_top, d_base) = crate::text::text_cap_band(theme::size::CAPTION, 0);
                let (t_cap, d_cap) = (t_base - t_top, d_base - d_top); // cap heights
                let pair_gap = ROW_SUB_GAP; // title baseline → detail cap-top
                let pair_top = sy + (h - (t_cap + pair_gap + d_cap)) * 0.5; // title cap-top
                (pair_top - t_top, pair_top + t_cap + pair_gap - d_top, pair_top + t_cap * 0.5)
            } else {
                (0.0, 0.0, cyc) // title_y/detail_y unused single-line; Label centres the label
            };
            // title/label, then inline badges (mockup: "Original: …" with an AD chip after it)
            let lbl = crate::text::elide(&row.label, text_right - label_x - trailing - badge_reserve, theme::size::HEADLINE, 1, false);
            let mut bx = label_x;
            if let Ok(cs) = CString::new(lbl) {
                bx += if two_line {
                    p.text(cs.as_ptr(), label_x, title_y, theme::size::HEADLINE, base, 0, 1)
                } else {
                    Label::new(cs.as_ptr(), theme::size::HEADLINE, base).bold().draw(p, Rect::new(label_x, sy, 0.0, h))
                };
            }
            bx += 12.0;
            for b in row.badges.iter() {
                // the shared chip leaf, in this row's contextual colours (ink over pill/panel)
                let sty = crate::ui::widgets::BadgeStyle::Outlined { col: base, bg: row_bg };
                bx += crate::ui::widgets::badge(p, bx, bcy, b.text(), sty) + 10.0;
            }
            // detail sub-line, elided (long Cyrillic descriptors would run off the edge)
            if !row.detail.is_empty() {
                let sub = if focused { [0.0f32, 0.0, 0.0, 0.6] } else { dimc };
                let detail = crate::text::elide(&row.detail, text_right - label_x, theme::size::CAPTION, 0, false);
                if let Ok(cd) = CString::new(detail) {
                    p.text(cd.as_ptr(), label_x, detail_y, theme::size::CAPTION, sub, 0, 0);
                }
            }
        });

        p.clip_clear();
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

