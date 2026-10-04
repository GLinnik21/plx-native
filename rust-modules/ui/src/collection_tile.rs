//! The **neutral collection tile** — what a collection with no artwork of its own draws in place of
//! a poster: the placeholder ground, the collection mark, and the collection's name under it.
//!
//! A collection whose server sent no `thumb` has nothing to resolve, so without this the tile is a
//! bare skeleton that reads as "still loading" forever and, beside its named neighbours, as a
//! broken image. The name is the one fact that tells two such tiles apart, so it is drawn ON the
//! tile rather than left to the focused caption, which only one tile at a time wears.
//!
//! **One widget for every surface that lists collections.** It is reached through the shared card
//! composite (`widgets::card`'s poster arm hands a thumb-less collection row here), so the Library
//! grid, a Home shelf and Search draw the same tile without asking. It names no application type:
//! the caller decides that a row is a thumb-less collection and passes the name.
//!
//! A server's automatic 2×2 composite is replaced by the poster store with a baked FAN of the
//! first members (`app::adapters::poster::fan`). The fan leaves the name out of its pixels: the
//! card sets it LIVE over the bake through [`draw_fan_name`], so a fan tile is told apart from its
//! neighbours unfocused too, exactly as this neutral tile is.
//!
//! Both names are set by ONE fitting policy, [`fit_name`]: sized, cased and wrapped from the tile's
//! RESTING width and memoised, so a focus pop moves the laid-out lines rather than re-wrapping them.
//! It measures through the live-font `widgets::LegacyMeasure` (`card` is drawn by callers that hold
//! no `Measure`, and the card composite skips recording passes, so no recorded layout depends on
//! this text); tests hand it the shipped faces' real advances.
use crate::icons::{self, Icon};
use crate::label::{HAlign, Label, VAlign};
use plx_machine::machine::Measure;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::os::raw::c_int;
use std::rc::Rc;
use crate::{theme, Painter, Rect};

/// The mock's grid tile width (`Collections.dc.html` G1, a 250×375 poster): the unit the FAN's
/// geometry is written in, scaled by the tile's resting width over it (the fan is a picture).
const MOCK_W: f32 = 250.0;
/// The narrowest mock tile that sets the name at full size: C1's 200-wide header. The mock writes
/// the name's type in fixed px (`.art b { font: 700 24px/1.08; left:18px; right:18px;
/// bottom:22px }`) on both its 200 and 250 tiles, so the name is the SAME size on both; only a tile
/// narrower than this scales it down.
const TYPE_FULL_W: f32 = 200.0;

// ── The fan's own geometry, shared with the bake (`app::adapters::poster::fan`) so the name's
// room is computed from the members the bake actually draws.

/// `.mc { width:44%; height:44% }` — a member's box, as a fraction of the tile (before the deck's
/// scale).
const FAN_MEMBER_FRAC: f32 = 0.44;

/// One member of the fan deck as the mock places it (`.c1`–`.c3`): its box's `left`/`top` as
/// fractions of the tile and the sine of its tilt (positive = clockwise on screen). CSS rotates
/// about the box's centre.
#[derive(Clone, Copy, Debug)]
pub struct FanMember {
    pub left: f32,
    pub top: f32,
    pub sin: f32,
}

/// sin(9°) and sin(8°): `.c1 { rotate(-9deg) }` and `.c2 { rotate(8deg) }`. Constants, so no
/// transcendental is evaluated at bake time (`ci/allow/libm.txt`).
const SIN_9: f32 = 0.156_434_46;
const SIN_8: f32 = 0.139_173_1;
/// `.c1` — the collection's SECOND member, back left.
pub const FAN_BACK_LEFT: FanMember = FanMember { left: 0.18, top: 0.14, sin: -SIN_9 };
/// `.c2` — the THIRD member, back right, over `.c1`.
pub const FAN_BACK_RIGHT: FanMember = FanMember { left: 0.40, top: 0.12, sin: SIN_8 };
/// `.c3` — the FIRST member, upright, on top.
pub const FAN_FRONT: FanMember = FanMember { left: 0.28, top: 0.10, sin: 0.0 };
const FAN_DECK: [FanMember; 3] = [FAN_BACK_LEFT, FAN_BACK_RIGHT, FAN_FRONT];

/// The deck's size over the mock's, one uniform linear scale about the deck's centre: every member
/// keeps its aspect, tilt and offset from the others. The owner asked for a bigger deck from sofa
/// distance, starting at +12%. The ceiling is the name's band on the 200-wide header (C1): past
/// about +11% a three-line name there no longer fits even at the step-down size with its half line
/// of air above and below, and elides; +10% keeps it whole with a margin. The rotated back members
/// stay well inside the mock's 18px side inset.
pub const FAN_DECK_SCALE: f32 = 1.10;

/// A member's box as drawn on a `w`×`h` tile: centre, half-extents and tilt.
#[derive(Clone, Copy, Debug)]
pub struct PlacedMember {
    pub cx: f32,
    pub cy: f32,
    pub hw: f32,
    pub hh: f32,
    pub sin: f32,
}

impl PlacedMember {
    pub fn cosine(&self) -> f32 {
        (1.0 - self.sin * self.sin).sqrt()
    }

    /// The rotated box's axis-aligned half-extents (its sharp corners — the 6px rounding only
    /// pulls them in).
    pub fn reach(&self) -> (f32, f32) {
        let (s, c) = (self.sin.abs(), self.cosine());
        (self.hw * c + self.hh * s, self.hw * s + self.hh * c)
    }
}

fn mock_member(m: &FanMember, w: f32, h: f32) -> PlacedMember {
    let (hw, hh) = (FAN_MEMBER_FRAC * w / 2.0, FAN_MEMBER_FRAC * h / 2.0);
    PlacedMember { cx: m.left * w + hw, cy: m.top * h + hh, hw, hh, sin: m.sin }
}

/// Where member `m` is drawn on a `w`×`h` tile: the mock's deck scaled by [`FAN_DECK_SCALE`] about
/// the centre of its rotated bounds, and that centre put on the tile's vertical axis.
pub fn fan_member(m: &FanMember, w: f32, h: f32) -> PlacedMember {
    let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for d in &FAN_DECK {
        let p = mock_member(d, w, h);
        let (rx, ry) = p.reach();
        x0 = x0.min(p.cx - rx);
        x1 = x1.max(p.cx + rx);
        y0 = y0.min(p.cy - ry);
        y1 = y1.max(p.cy + ry);
    }
    let (bx, by) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    let p = mock_member(m, w, h);
    let s = FAN_DECK_SCALE;
    PlacedMember { cx: w * 0.5 + s * (p.cx - bx), cy: by + s * (p.cy - by), hw: s * p.hw, hh: s * p.hh,
        sin: p.sin }
}

/// The lowest point of any member on a `w`×`h` tile, rotated corners included.
pub fn fan_deck_bottom(w: f32, h: f32) -> f32 {
    FAN_DECK.iter().map(|m| {
        let p = fan_member(m, w, h);
        p.cy + p.reach().1
    }).fold(f32::MIN, f32::max)
}

/// `.scr { height:45% }` — the bottom scrim starts at 55% of the tile's height.
pub const FAN_SCRIM_FROM: f32 = 0.55;
/// How far below the front member's box its drop shadow reaches, in mock px: `.mc { box-shadow:
/// 0 6px 14px … }` — the 6px offset plus half the 14px blur, where it has faded to nothing visible.
const FAN_SHADOW_REACH: f32 = 6.0 + 7.0;


/// How a tile sets a name, in mock px (full size from [`TYPE_FULL_W`] up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NameStyle {
    /// The normal size, every name's first try (never grown to fill a short name).
    pub size: f32,
    /// The one smaller size a name that overflows its band steps down to.
    pub step: f32,
    /// The least a single over-wide word is shrunk to before it is elided.
    pub floor: f32,
    /// Side inset of the text column.
    pub side: f32,
    /// Line pitch over the size (CSS `line-height`).
    pub lead: f32,
    /// Upper-case the name (Unicode-aware; a caseless script passes through).
    pub upper: bool,
    /// Clear air at the top of the name's band, in lines of its own pitch (the fan keeps half a
    /// line between its front member and the block).
    pub gap: f32,
}

/// G1's `.art b`: `font: 700 24px/1.08`, `left:18px; right:18px`, upper-case.
pub const FAN_NAME: NameStyle =
    NameStyle { size: 24.0, step: 20.0, floor: 18.0, side: 18.0, lead: 1.08, upper: true, gap: 0.5 };
/// G1's `.ety div`: `font: 700 24px/1.12`, the neutral tile's 10% column inset, as written.
pub const NEUTRAL_NAME: NameStyle =
    NameStyle { size: 24.0, step: 20.0, floor: 18.0, side: 25.0, lead: 1.12, upper: false, gap: 0.0 };
/// `bottom:22px` — the least air under the name block, in mock px.
const NAME_BOTTOM: f32 = 22.0;

/// One line of a fitted name: its text and the size it is drawn at (a shrunk over-wide word is
/// smaller than its block).
#[derive(Clone, Debug, PartialEq)]
pub struct FittedLine {
    pub text: String,
    pub sz: c_int,
}

/// A name set for one resting tile: its lines, the block's size, and its line pitch.
#[derive(Clone, Debug, PartialEq)]
pub struct FittedName {
    pub lines: Vec<FittedLine>,
    pub sz: c_int,
    pub pitch: f32,
    /// The text column's width.
    pub column: f32,
}

impl FittedName {
    /// The line boxes' height (CSS: lines × line-height).
    pub fn height(&self) -> f32 {
        self.lines.len() as f32 * self.pitch
    }

    /// The INK's height: line 0's cap top to the last line's baseline.
    pub fn ink(&self, m: &dyn Measure) -> f32 {
        if self.lines.is_empty() { return 0.0; }
        (self.lines.len() - 1) as f32 * self.pitch + m.cap_h(self.sz)
    }
}

/// The name's type scale on a tile `rest_w` wide at rest: 1 from [`TYPE_FULL_W`] up.
fn type_k(rest_w: f32) -> f32 {
    (rest_w / TYPE_FULL_W).min(1.0)
}

/// Greedy word wrap at `w`: a word wider than `w` takes a line of its own, unsplit.
fn greedy(words: &[&str], w: f32, sz: c_int, m: &dyn Measure) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in words {
        if cur.is_empty() {
            cur.push_str(word);
            continue;
        }
        let trial = format!("{cur} {word}");
        if m.width_str(&trial, sz, true) <= w {
            cur = trial;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// `text-wrap: balance`: the narrowest whole-pixel column that keeps greedy's line count, so no
/// lone orphan word hangs under a full line when a more even break exists.
fn balanced(words: &[&str], column: f32, sz: c_int, m: &dyn Measure) -> Vec<String> {
    let lines = greedy(words, column, sz, m);
    if lines.len() < 2 {
        return lines;
    }
    let widest = words.iter().map(|w| m.width_str(w, sz, true)).fold(0.0f32, f32::max);
    let (mut lo, mut hi) = (widest.ceil() - 1.0, column.floor());
    while hi - lo > 1.0 {
        let mid = ((lo + hi) * 0.5).floor();
        if greedy(words, mid, sz, m).len() <= lines.len() { hi = mid } else { lo = mid }
    }
    greedy(words, hi, sz, m)
}

/// Elide `text` to `w` at `sz` with a trailing ellipsis.
fn elide(text: &str, w: f32, sz: c_int, m: &dyn Measure) -> String {
    plx_gfx::text::elide_by(text, w, false, |t| m.width_str(t, sz, true))
}

/// Set line `text` at `sz`, or — a single word wider than the column — shrink it toward `floor`
/// and elide what still does not fit. Never wider than the column.
fn set_line(text: String, sz: c_int, floor: c_int, column: f32, m: &dyn Measure) -> FittedLine {
    let w = m.width_str(&text, sz, true);
    if w <= column {
        return FittedLine { text, sz };
    }
    let mut small = sz;
    while small > floor && m.width_str(&text, small, true) > column {
        small -= 1;
    }
    let text = if m.width_str(&text, small, true) > column { elide(&text, column, small, m) } else { text };
    FittedLine { text, sz: small }
}

/// **The one name-fitting policy** of the fan and the neutral tile, for a tile `rest_w` wide at
/// rest whose name may use a band `band` tall:
///
/// 1. the type is the mock's fixed px (24/1.08, 18px side insets) — the same on a 200 and a 250
///    tile, as the mock sets it — scaled down only on a tile narrower than [`TYPE_FULL_W`];
/// 2. a name that fits on one line stays one centred line at the NORMAL size — never grown;
/// 3. a longer one is balance-wrapped (`text-wrap: balance`) into as many lines as the band holds
///    at that size — its ink plus the style's clear `gap` above and below — with no fixed line cap;
/// 4. only if that overflows the band: the ONE smaller size, again as many lines as fit;
/// 5. only if that still overflows: the last line that fits ends in an ellipsis;
/// 6. a single word wider than the column shrinks toward the style's floor, then elides;
/// 7. upper-case is Unicode's (`be` Cyrillic works; a caseless script passes through);
/// 8. an empty or blank name sets nothing. Plex's " Collection" suffix is kept.
pub fn fit_name(name: &str, style: &NameStyle, rest_w: f32, band: f32,
    m: &dyn Measure) -> FittedName {
    let k = type_k(rest_w);
    let px = |v: f32| (v * k).round().max(1.0) as c_int;
    let (normal, step, floor) = (px(style.size), px(style.step), px(style.floor).min(px(style.step)));
    let column = (rest_w - 2.0 * style.side * k).max(1.0).floor();
    let text = if style.upper { name.to_uppercase() } else { name.to_owned() };
    let words: Vec<&str> = text.split_whitespace().collect();
    let pitch = |sz: c_int| sz as f32 * style.lead;
    // Lines that fit: the block's INK (line 0's cap top to the last baseline) plus the clear gap
    // above AND below it — it is centred — within the band.
    let room = |sz: c_int| {
        let spare = band - 2.0 * style.gap * pitch(sz) - m.cap_h(sz);
        ((spare / pitch(sz)).floor().max(0.0) as usize + 1).max(1)
    };
    if words.is_empty() {
        return FittedName { lines: Vec::new(), sz: normal, pitch: pitch(normal), column };
    }
    for sz in [normal, step] {
        if greedy(&words, column, sz, m).len() <= room(sz) {
            let lines = balanced(&words, column, sz, m).into_iter()
                .map(|l| set_line(l, sz, floor, column, m)).collect();
            return FittedName { lines, sz, pitch: pitch(sz), column };
        }
    }
    let mut wrapped = greedy(&words, column, step, m);
    let rest = wrapped.split_off(room(step) - 1).join(" ");
    let last = elide(&rest, column, step, m);
    wrapped.push(if last.is_empty() { "\u{2026}".into() } else { last });
    let lines = wrapped.into_iter().map(|l| set_line(l, step, floor, column, m)).collect();
    FittedName { lines, sz: step, pitch: pitch(step), column }
}

/// The band a fan's name is centred in on a `rest` tile, as (top, height) from the tile's top:
/// from the deck's lowest point — the rotated back members' corners included — and its drop
/// shadow (and never above the scrim) down to the mock's bottom inset. The fan's geometry scales
/// with the tile; the inset is type, like the name.
pub fn fan_band(rest: Rect) -> (f32, f32) {
    let fan_k = rest.w / MOCK_W;
    let deck = fan_deck_bottom(rest.w, rest.h) + FAN_SHADOW_REACH * fan_k;
    let top = deck.max(FAN_SCRIM_FROM * rest.h);
    let bottom = rest.h - NAME_BOTTOM * type_k(rest.w);
    (top, (bottom - top).max(0.0))
}

/// The band the neutral tile's name may use: the tile less the mark, its gap and the mock's inset
/// above and below.
fn neutral_band(rest: Rect) -> f32 {
    (rest.h - glyph_px(rest.w) - rest.w * GAP_RATIO - 2.0 * NAME_BOTTOM * type_k(rest.w)).max(0.0)
}

thread_local! {
    /// Per (name, style, resting size): the fitted name. Settled once per resting width, so a frame
    /// pays one lookup per tile and a focus pop never re-wraps.
    static FITTED: RefCell<HashMap<u64, Rc<FittedName>>> = RefCell::new(HashMap::new());
}
/// A bound on [`FITTED`]; past it the memo starts over (a library's collections are far fewer).
const FITTED_CAP: usize = 512;

fn fitted(name: &str, style: &NameStyle, rest: Rect, band: f32) -> Rc<FittedName> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (name, style.upper, style.side.to_bits(), style.lead.to_bits(), rest.w.to_bits(),
        rest.h.to_bits(), band.to_bits()).hash(&mut h);
    let id = h.finish();
    if let Some(hit) = FITTED.with(|m| m.borrow().get(&id).cloned()) {
        return hit;
    }
    let fit = Rc::new(fit_name(name, style, rest.w, band, &crate::widgets::LegacyMeasure));
    FITTED.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() >= FITTED_CAP {
            m.clear();
        }
        m.insert(id, Rc::clone(&fit));
    });
    fit
}

/// Draw `fit`'s lines centred on `cx`, line 0's cap top at `top`; a shrunk line shares its block
/// line's baseline.
fn draw_lines(p: Painter, fit: &FittedName, cx: f32, top: f32, col: [f32; 4]) {
    let cap = plx_machine::machine::Measure::cap_h(&crate::widgets::LegacyMeasure, fit.sz);
    for (i, line) in fit.lines.iter().enumerate() {
        let Ok(text) = CString::new(line.text.as_str()) else { continue };
        let baseline = top + i as f32 * fit.pitch + cap;
        Label::new(text.as_ptr(), line.sz, col).bold().h(HAlign::Center).v(VAlign::Baseline)
            .draw(p, Rect::new(cx - fit.column * 0.5, baseline, fit.column, 0.0));
    }
}

/// The mark's size as a fraction of the tile's width.
const GLYPH_RATIO: f32 = 0.3;
/// Air between the mark and the name's cap band, as a fraction of the tile's width.
const GAP_RATIO: f32 = 0.08;

/// The mark's raster size for a tile `w` wide — quantized to 4px so a focus pop reuses a handful
/// of cached masks instead of rasterizing one per rounded pixel (the person glyph's discipline).
pub fn glyph_px(w: f32) -> f32 {
    ((w * GLYPH_RATIO) / 4.0).round() * 4.0
}

/// Where the mark and the name block sit in `r`: the pair is centred vertically as one block,
/// the mark above, sized from the tile so a scaled (popped) tile scales its layout with it.
pub fn layout(r: Rect, name_w: f32, name_h: f32) -> (Rect, Rect) {
    let d = glyph_px(r.w);
    let gap = r.w * GAP_RATIO;
    let top = r.y + (r.h - (d + gap + name_h)) * 0.5;
    let glyph = Rect::new(r.cx() - d * 0.5, top, d, d);
    let name = Rect::new(r.cx() - name_w * 0.5, top + d + gap, name_w, name_h);
    (glyph, name)
}

/// Draw the neutral tile at `r` (the rect it is drawn at, popped when focused) with corner radius
/// `rad`; `rest` is its RESTING rect, which the name is fitted to. The caller has already decided
/// the row is a collection with no artwork (`thumb` empty) — an unresolved texture with a path
/// behind it is merely still loading and keeps the ordinary skeleton.
pub fn draw(p: Painter, rest: Rect, r: Rect, rad: f32, name: &str) {
    p.rrect_sheened(r, rad, theme::CARD_PLACEHOLDER);
    let fit = fitted(name, &NEUTRAL_NAME, rest, neutral_band(rest));
    let (glyph, column) = layout(r, fit.column, fit.height());
    icons::draw(p, Icon::Collection, glyph, theme::TEXT_TERTIARY);
    draw_lines(p, &fit, column.cx(), column.y + NAME_CAP_DROP * fit.sz as f32, theme::TEXT_SECONDARY);
}

// ── The name on a baked fan: G1's `.art b`, set by [`fit_name`] and centred under the fan.

/// `text-shadow: 0 2px …` — the drop's offset, in mock px.
const FAN_NAME_SHADOW_DY: f32 = 2.0;
/// Where CSS puts the cap top inside a ~1.1 line box, in em: the half-leading plus the font's
/// ascent-over-cap-height, about 0.15.
const NAME_CAP_DROP: f32 = 0.15;

/// Where a fitted fan name's cap top sits on a `rest` tile, from the tile's top: its INK (line 0's
/// cap top to the last baseline) centred in [`fan_band`], equal air above and below — one line in
/// the middle of the band, a full block filling it, and (by [`fit_name`]'s room) never closer than
/// half a line to the deck. The fan never moves.
pub fn fan_name_top(rest: Rect, fit: &FittedName, m: &dyn Measure) -> f32 {
    let (top, band) = fan_band(rest);
    top + (band - fit.ink(m)) * 0.5
}

/// Set a collection's NAME over its baked fan, as G1 does: [`fit_name`]'s block centred in the band
/// between the front member and the bottom inset, over a drop.
///
/// `rest` is the tile's RESTING rect and `r` the rect it is drawn at (popped when focused). The
/// fit comes from `rest`, so a focus pop never re-wraps; the block's centre rides `r`'s scale so it
/// moves with the art.
///
/// **The caller has already decided the thumb IS a baked fan** — whether a path is the server's
/// generated composite is the application's fact (`TileFacts::composite_thumb`), so this draws the
/// name unconditionally. A custom poster is artwork and carries no name: the caller does not call.
pub fn draw_fan_name(p: Painter, rest: Rect, r: Rect, name: &str) {
    let fit = fitted(name, &FAN_NAME, rest, fan_band(rest).1);
    if fit.lines.is_empty() {
        return;
    }
    let m = crate::widgets::LegacyMeasure;
    let ink = fit.ink(&m);
    let centre = (fan_name_top(rest, &fit, &m) + ink * 0.5) * (r.h / rest.h.max(1.0));
    let top = r.y + centre - ink * 0.5;
    let k = type_k(rest.w);
    draw_lines(p, &fit, r.cx(), top + FAN_NAME_SHADOW_DY * k, theme::FAN_NAME_SHADOW);
    draw_lines(p, &fit, r.cx(), top, theme::FAN_NAME_INK);
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_base::fontcov::advances::ShippedMeasure;

    fn tile(w: f32) -> Rect { Rect::new(0.0, 0.0, w, w * 1.5) }
    fn fit(name: &str, w: f32) -> FittedName {
        fit_name(name, &FAN_NAME, w, fan_band(tile(w)).1, &ShippedMeasure)
    }
    fn texts(f: &FittedName) -> Vec<&str> { f.lines.iter().map(|l| l.text.as_str()).collect() }
    fn fits_column(f: &FittedName) -> bool {
        f.lines.iter().all(|l| ShippedMeasure.width_str(&l.text, l.sz, true) <= f.column)
    }
    fn elided(f: &FittedName) -> bool { f.lines.iter().any(|l| l.text.ends_with('\u{2026}')) }
    /// The ink's centre, relative to the band's centre.
    fn off_centre(f: &FittedName, w: f32) -> f32 {
        let (top, band) = fan_band(tile(w));
        fan_name_top(tile(w), f, &ShippedMeasure) + f.ink(&ShippedMeasure) * 0.5 - (top + band * 0.5)
    }
    /// The deck's lowest and side-most points, from each member's four corners rotated about its
    /// centre — computed here independently of `PlacedMember::reach`.
    fn corners(w: f32) -> (f32, f32, f32) {
        let (mut left, mut right, mut bottom) = (f32::MAX, f32::MIN, f32::MIN);
        for m in [FAN_BACK_LEFT, FAN_BACK_RIGHT, FAN_FRONT] {
            let p = fan_member(&m, w, w * 1.5);
            for (dx, dy) in [(-p.hw, -p.hh), (p.hw, -p.hh), (-p.hw, p.hh), (p.hw, p.hh)] {
                let (x, y) = (p.cx + dx * p.cosine() - dy * p.sin, p.cy + dx * p.sin + dy * p.cosine());
                left = left.min(x); right = right.max(x); bottom = bottom.max(y);
            }
        }
        (left, right, bottom)
    }
    /// The band's top as the owner defines it: the lowest corner plus the shadow.
    fn deck_line(w: f32) -> f32 { corners(w).2 + FAN_SHADOW_REACH * w / MOCK_W }

    #[test]
    fn the_mark_and_name_are_one_block_centred_on_the_tile() {
        let r = Rect::new(100.0, 200.0, 250.0, 375.0);
        let (glyph, name) = layout(r, 200.0, 64.0);
        assert_eq!(glyph.w, 76.0, "a grid poster's mark quantizes to 76px");
        assert!((glyph.cx() - r.cx()).abs() < 0.01);
        let top_air = glyph.y - r.y;
        let bottom_air = r.y + r.h - (name.y + name.h);
        assert!((top_air - bottom_air).abs() < 0.01, "the block is centred: {top_air} vs {bottom_air}");
        assert!(name.y > glyph.y + glyph.h, "the name sits under the mark");
    }

    #[test]
    fn a_pop_reuses_quantized_mark_sizes() {
        let sizes: std::collections::BTreeSet<i32> = (0..=20)
            .map(|step| glyph_px(250.0 * (1.0 + step as f32 * 0.004)) as i32)
            .collect();
        assert!(sizes.len() <= 2, "a 1.08 pop spans at most two cached masks: {sizes:?}");
        assert!(sizes.iter().all(|px| px % 4 == 0));
    }

    /// The mock's type is fixed px: 24/1.08 with 18px insets on BOTH its 200-wide header (C1) and
    /// its 250-wide grid tile (G1) — C1's baselines are 26px apart — and only a narrower tile scales.
    #[test]
    fn the_name_is_the_mocks_fixed_type() {
        for (w, column) in [(200.0, 164.0), (250.0, 214.0)] {
            let f = fit("Up", w);
            assert_eq!((f.sz, f.column), (24, column), "{w}");
            assert!((f.pitch - 25.92).abs() < 0.01, "{w}: 1.08 line height, not the font's own: {}", f.pitch);
        }
        assert_eq!(fit("Up", 150.0).sz, 18, "a tile narrower than the header scales down");
    }

    /// The band runs from the deck's lowest point — the rotated back members' corners — and its
    /// shadow down to the mock's 22px inset; a squat tile has less of it, so the same name gets
    /// fewer lines there.
    #[test]
    fn the_band_starts_under_the_decks_lowest_corner_and_its_shadow() {
        for w in [200.0, 250.0] {
            let (top, band) = fan_band(tile(w));
            assert!((top - deck_line(w)).abs() < 0.01, "{w}: band top {top} vs lowest corner + shadow {}", deck_line(w));
            assert!((top + band - (w * 1.5 - 22.0)).abs() < 0.01, "{w}: the bottom inset is 22px");
        }
        let squat = Rect::new(0.0, 0.0, 250.0, 250.0);
        assert!(fan_band(squat).1 < fan_band(tile(250.0)).1);
        let long = "The Complete Blender Foundation Open Movie Projects Archive Collection";
        let in_squat = fit_name(long, &FAN_NAME, 250.0, fan_band(squat).1, &ShippedMeasure);
        assert!(in_squat.ink(&ShippedMeasure) <= fan_band(squat).1 + 0.01 && elided(&in_squat), "{:?}", texts(&in_squat));
    }

    /// The deck is the mock's scaled +10% about its centre — same tilts, same aspect, same relative
    /// offsets — centred on the tile, and its rotated corners keep the mock's 18px side inset.
    #[test]
    fn the_deck_is_the_mocks_scaled_uniformly_and_centred() {
        for w in [200.0, 250.0] {
            let (left, right, _) = corners(w);
            let k = w / MOCK_W;
            assert!(left >= 18.0 * k && w - right >= 18.0 * k, "{w}: side air {left} / {}", w - right);
            assert!((left - (w - right)).abs() < 0.01, "{w}: centred");
            let (a, b) = (fan_member(&FAN_FRONT, w, w * 1.5), fan_member(&FAN_BACK_LEFT, w, w * 1.5));
            let (ma, mb) = (mock_member(&FAN_FRONT, w, w * 1.5), mock_member(&FAN_BACK_LEFT, w, w * 1.5));
            assert!((a.hw / ma.hw - FAN_DECK_SCALE).abs() < 1e-4 && (a.hh / a.hw - ma.hh / ma.hw).abs() < 1e-4);
            assert!(((a.cx - b.cx) / (ma.cx - mb.cx) - FAN_DECK_SCALE).abs() < 1e-3, "{w}: offsets scale with it");
            assert!(((a.cy - b.cy) / (ma.cy - mb.cy) - FAN_DECK_SCALE).abs() < 1e-3);
            assert_eq!(b.sin, -SIN_9);
        }
        assert_eq!(FAN_DECK_SCALE, 1.10);
    }

    /// **Centred on the INK**: from the band's top (the deck's lowest corner plus shadow) to line 0's
    /// cap top equals the last baseline to the band's bottom, within 2px — for a one-line, a
    /// three-line and a long name (as many lines as the band holds), at both widths.
    #[test]
    fn the_names_ink_is_centred_between_the_deck_and_the_inset() {
        for w in [200.0, 250.0] {
            for name in ["Cars", "Blender Studio Anniversary Collection",
                "The Complete Blender Foundation Open Movie Projects Archive Collection"] {
                let f = fit(name, w);
                let top = fan_name_top(tile(w), &f, &ShippedMeasure);
                let bottom = top + f.ink(&ShippedMeasure);
                let above = top - deck_line(w);
                let below = (w * 1.5 - 22.0) - bottom;
                assert!((above - below).abs() <= 2.0, "{w} {name}: {above} above vs {below} below");
                assert!(above >= 0.5 * f.pitch - 0.01, "{w} {name}: {above} is under half a line");
            }
        }
    }

    /// The deck grew; a typical three-line name still sets at the full 24px on a grid tile.
    #[test]
    fn a_three_line_name_still_sets_at_full_size_on_the_grid() {
        let f = fit("Blender Studio Anniversary Collection", 250.0);
        assert_eq!((f.lines.len(), f.sz), (3, 24), "{:?}", texts(&f));
        assert!(!elided(&f));
    }

    /// A one-word name is one line at the normal size — never grown — in the MIDDLE of the band.
    #[test]
    fn a_short_name_is_one_line_centred_in_the_band() {
        for w in [200.0, 250.0] {
            let f = fit("Cars", w);
            assert_eq!(texts(&f), ["CARS"]);
            assert_eq!((f.lines[0].sz, f.sz), (24, 24));
            assert!(off_centre(&f, w).abs() < 0.01, "{w}: {}", off_centre(&f, w));
        }
    }

    /// A typical name sets in BALANCED lines: two at 24px on G1's 250 tile, as the mock does; on the
    /// header's 200 tile the band under the bigger deck holds two at the step-down size. No
    /// narrower column keeps either count.
    #[test]
    fn a_typical_name_balances_its_lines() {
        for (w, n, sz) in [(200.0, 2, 20), (250.0, 2, 24)] {
            let f = fit("Harbor Lights Mysteries", w);
            assert_eq!((f.lines.len(), f.sz), (n, sz), "{w}: {:?}", texts(&f));
            assert!(!elided(&f));
            assert!(fits_column(&f) && off_centre(&f, w).abs() < 0.01);
            let widths: Vec<f32> = f.lines.iter().map(|l| ShippedMeasure.width_str(&l.text, l.sz, true)).collect();
            let greedy = greedy(&["HARBOR", "LIGHTS", "MYSTERIES"], f.column, f.sz, &ShippedMeasure);
            let greedy_max = greedy.iter().map(|l| ShippedMeasure.width_str(l, f.sz, true)).fold(0.0, f32::max);
            assert!(widths.iter().cloned().fold(0.0, f32::max) <= greedy_max, "{w}: balanced is never wider than greedy");
        }
    }

    /// C1: the header's name breaks as the mock does — three whole lines on the 200 tile — at the
    /// step-down size the band under the bigger deck holds.
    #[test]
    fn the_headers_name_takes_three_lines_like_the_mock() {
        let f = fit("Starfall Saga Collection", 200.0);
        assert_eq!(texts(&f), ["STARFALL", "SAGA", "COLLECTION"]);
        assert_eq!(f.sz, 20);
    }

    /// How many lines the band holds at `sz`: the ink plus half a line of air above and below.
    fn room(w: f32, sz: i32) -> usize {
        let (_, band) = fan_band(tile(w));
        let pitch = sz as f32 * FAN_NAME.lead;
        ((band - pitch - ShippedMeasure.cap_h(sz)) / pitch).floor() as usize + 1
    }

    /// A long name uses every line the band holds — there is no line cap — and renders complete
    /// with no ellipsis while it fits: at the normal size if it can, else at the one step down.
    #[test]
    fn a_long_name_renders_complete_while_it_fits_the_band() {
        let long = "Blender Foundation Open Movie Archive Collection";
        let f = fit(long, 250.0);
        assert!(!elided(&f) && fits_column(&f), "{:?} at {}", texts(&f), f.sz);
        assert_eq!(texts(&f).join(" "), long.to_uppercase());
        assert!(f.lines.len() > 3, "no three-line cap: {:?} (room {})", texts(&f), room(250.0, f.sz));
        let words: Vec<String> = long.to_uppercase().split_whitespace().map(str::to_owned).collect();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let at_normal = greedy(&words, f.column, 24, &ShippedMeasure).len();
        assert_eq!(f.sz, if at_normal <= room(250.0, 24) { 24 } else { 20 }, "the normal size unless it overflows");
        assert!(off_centre(&f, 250.0).abs() < 0.01);
    }

    /// The block never crowds the fan: its cap top sits at least half a line below the front
    /// member's bottom edge and the reach of its drop shadow, however long the name.
    #[test]
    fn a_long_names_block_keeps_clear_of_the_front_member() {
        let long = "The Complete Blender Foundation Open Movie Projects Archive Collection";
        for w in [200.0, 250.0] {
            let f = fit(long, w);
            let clear = deck_line(w) + 0.5 * f.pitch;
            let top = fan_name_top(tile(w), &f, &ShippedMeasure);
            assert!(top >= clear - 0.01, "{w}: block top {top} above {clear}: {:?} at {}", texts(&f), f.sz);
            assert!(fits_column(&f));
        }
    }

    /// Past the band at the normal size the name steps down ONCE, uses as many lines as fit there,
    /// and only then ends the last line that fits in an ellipsis.
    #[test]
    fn an_overflowing_name_steps_down_once_then_elides() {
        let long = "The Complete and Utterly Definitive Chronological Anthology of Every Starfall Film Ever Made In Any Format";
        for w in [200.0, 250.0] {
            let f = fit(long, w);
            assert_eq!(f.sz, 20, "{w}");
            assert_eq!(f.lines.len(), room(w, f.sz), "{w}: every line the band holds");
            assert!(f.lines.last().unwrap().text.ends_with('\u{2026}'), "{w}: {:?}", texts(&f));
            assert!(fits_column(&f));
        }
    }

    /// A single word wider than the column shrinks toward the floor, then elides — never wider.
    #[test]
    fn an_overwide_word_shrinks_to_the_floor_then_elides() {
        for w in [200.0, 250.0] {
            let f = fit("Donaudampfschifffahrtsgesellschaft", w);
            assert_eq!(f.lines.len(), 1);
            assert!(f.lines[0].sz < f.sz && f.lines[0].sz >= 18, "{w}: {:?}", f.lines);
            assert!(fits_column(&f), "{w}: {:?}", f.lines);
        }
        let f = fit("Rindfleischetikettierungsüberwachungsaufgabenübertragungsgesetz", 250.0);
        assert_eq!(f.lines[0].sz, 18);
        assert!(f.lines[0].text.ends_with('\u{2026}') && fits_column(&f));
    }

    #[test]
    fn a_blank_name_sets_nothing() {
        for w in [200.0, 250.0] {
            assert!(fit("", w).lines.is_empty());
            assert!(fit("  \t ", w).lines.is_empty());
        }
    }

    /// Unicode upper-case: a Cyrillic name is cased like a Latin one; " Collection" is kept.
    #[test]
    fn a_cyrillic_name_is_upper_cased_and_fits() {
        for w in [200.0, 250.0] {
            let f = fit("Калекцыя Зорнага Падзення", w);
            assert!(texts(&f).join(" ") == "КАЛЕКЦЫЯ ЗОРНАГА ПАДЗЕННЯ", "{w}: {:?}", texts(&f));
            assert!(fits_column(&f));
            let kept = fit("Starfall Saga Collection", w);
            assert_eq!(texts(&kept).join(" "), "STARFALL SAGA COLLECTION");
        }
        let neutral = fit_name("Paper Kites", &NEUTRAL_NAME, 250.0, neutral_band(tile(250.0)), &ShippedMeasure);
        assert_eq!(texts(&neutral), ["Paper Kites"], "the neutral tile keeps the name's own case");
    }
}
