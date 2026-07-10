//! Reusable retui leaves + shared helpers. These are the "reusable UI elements":
//! PillButton, CircleButton, PageDots, ProgressBar (the future player-HUD
//! scrubber), plus the poster-resolve + text-wrap helpers Card/Hero share.
#![allow(dead_code)]
use crate::pms::PmsMovie;
use crate::ui::{Env, Painter, Rect, Size, Spring, View};
use std::os::raw::{c_char, c_int};

/// build the transcode key on the stack and resolve it to a GL texture (0 until loaded)
pub(crate) fn resolve_tex(path: *const c_char, w: c_int, h: c_int, png: c_int) -> u32 {
    let mut key = [0u8; 352];
    crate::posters::poster_key(key.as_mut_ptr() as *mut c_char, key.len(), path, w, h, png);
    crate::posters::poster_get(key.as_ptr() as *const c_char)
}

/// a poster card: artwork if loaded, else a dark skeleton (the Card + Grid draw op)
pub(crate) fn draw_poster(p: Painter, m: Option<&PmsMovie>, r: Rect, rad: f32) {
    const SK_T: [f32; 4] = [0.13, 0.14, 0.17, 1.0];
    const SK_B: [f32; 4] = [0.08, 0.09, 0.11, 1.0];
    if let Some(m) = m {
        if m.thumb[0] != 0 {
            let t = resolve_tex(m.thumb.as_ptr() as *const c_char, 250, 375, 0);
            if t != 0 {
                p.tex(t, r, rad, [1.0; 4]);
                return;
            }
        }
    }
    p.rect(r, rad, SK_T, SK_B, 0.0);
}

/// The scale a focused card pops to (shared by every animated card row).
pub(crate) const CARD_FOCUS_SCALE: f32 = 1.07;

/// A landscape media card shared by the episode picker and the chapters strip so they animate
/// identically: the thumbnail (resolved at `res`, or a dark placeholder until loaded), a focus
/// ring, and a focus **scale-pop** about the frame's centre (applied only when `focused`; the
/// caller owns the `scale` spring and pops it on selection change).
pub(crate) fn draw_card(p: Painter, frame: Rect, thumb: &str, res: (c_int, c_int), radius: f32, focused: bool, scale: f32) {
    let r = if focused { frame.scaled(scale) } else { frame };
    let mut drew = false;
    if !thumb.is_empty() {
        if let Ok(tp) = std::ffi::CString::new(thumb) {
            let t = resolve_tex(tp.as_ptr(), res.0, res.1, 0);
            if t != 0 {
                p.tex(t, r, radius, [1.0; 4]);
                drew = true;
            }
        }
    }
    if !drew {
        p.rrect(r, radius, radius, [0.12, 0.13, 0.16, 1.0]);
    }
    if focused {
        p.ring(r, 6.0, 14.0, 1.0);
    }
}

/// read a NUL-terminated C-string field into a Rust String
pub(crate) fn cfield(b: &[u8]) -> String {
    let n = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).into_owned()
}

/// wrap a synopsis to two lines on a word boundary (mirrors ui_home.c's rule)
pub(crate) fn wrap_two(s: &str) -> (String, String) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut brk = n;
    if n > 62 {
        brk = 62;
        while brk > 24 && bytes[brk] != b' ' {
            brk -= 1;
        }
    }
    let c1 = brk.min(87);
    let l1 = String::from_utf8_lossy(&bytes[..c1]).into_owned();
    if brk >= n {
        return (l1, String::new());
    }
    let s2 = &bytes[brk + 1..];
    let m = s2.len();
    let mut c2 = m;
    if m > 66 {
        c2 = 66;
        while c2 > 24 && s2[c2] != b' ' {
            c2 -= 1;
        }
    }
    c2 = c2.min(92);
    let mut l2 = String::from_utf8_lossy(&s2[..c2]).into_owned();
    if c2 < m {
        l2.push('\u{2026}'); // …
    }
    (l1, l2)
}

// ---- PillButton: rounded fill + optional play triangle + label, as a group ----
pub struct PillButton {
    pub frame: Rect,
    pub label: *const c_char,
    pub tri: bool,
    pub fill: [f32; 4],
    pub ink: [f32; 4],
}
impl PillButton {
    pub fn play(label: *const c_char) -> Self {
        Self { frame: Rect::new(0.0, 0.0, 168.0, 60.0), label, tri: true,
               fill: [0.97, 0.98, 0.99, 1.0], ink: [0.05, 0.06, 0.08, 1.0] }
    }
    pub fn at(mut self, x: f32, y: f32) -> Self {
        self.frame.x = x;
        self.frame.y = y;
        self
    }
}
impl View for PillButton {
    fn draw(&self, _e: &Env, p: Painter) {
        let r = self.frame;
        p.rrect(r, r.h * 0.5, r.h * 0.5, self.fill);
        if self.tri {
            let th = r.h * 0.40;
            p.ptri(Rect::new(r.x + 40.0, r.y + (r.h - th) * 0.5, th, th), self.ink);
        }
        p.text(self.label, r.x + 76.0, r.y + (r.h - 30.0) * 0.5 - 1.0, 30, self.ink, 0, 1);
    }
    fn measure(&self) -> Size {
        Size { w: self.frame.w, h: self.frame.h }
    }
}

// ---- CircleButton: circular face + centered glyph ----
pub struct CircleButton {
    pub frame: Rect,
    pub glyph: *const c_char,
    pub face: [f32; 4],
    pub ink: [f32; 4],
}
impl CircleButton {
    pub fn new(glyph: *const c_char) -> Self {
        Self { frame: Rect::new(0.0, 0.0, 60.0, 60.0), glyph,
               face: [0.42, 0.44, 0.50, 0.5], ink: [0.92, 0.94, 0.97, 1.0] }
    }
    pub fn at(mut self, x: f32, y: f32) -> Self {
        self.frame.x = x;
        self.frame.y = y;
        self
    }
}
impl View for CircleButton {
    fn draw(&self, _e: &Env, p: Painter) {
        let r = self.frame;
        let d = r.w;
        p.rect(r, d * 0.5, self.face, self.face, 0.0);
        p.text(self.glyph, r.x + d * 0.5, r.y + (d - 32.0) * 0.5 - 2.0, 32, self.ink, 1, 1);
    }
    fn measure(&self) -> Size {
        Size { w: self.frame.w, h: self.frame.h }
    }
}

// ---- PageDots: page indicators; active dot elongated ----
pub struct PageDots {
    pub count: usize,
    pub active: usize,
    pub x: f32,
    pub y: f32,
}
impl PageDots {
    pub fn new(count: usize) -> Self {
        Self { count, active: 0, x: 0.0, y: 0.0 }
    }
    pub fn at(mut self, x: f32, y: f32) -> Self {
        self.x = x;
        self.y = y;
        self
    }
}
impl View for PageDots {
    fn draw(&self, _e: &Env, p: Painter) {
        for d in 0..self.count {
            let dw = if d == self.active { 26.0 } else { 11.0 };
            let a = if d == self.active { 0.95 } else { 0.35 };
            let dc = [0.85, 0.87, 0.9, a];
            p.rect(Rect::new(self.x + d as f32 * 20.0, self.y, dw, 11.0), 5.5, dc, dc, 0.0);
        }
    }
}

// ---- Spinner: dots around a circle, the leading one bright and trailing into a fade. A loading/
// buffering indicator (e.g. the player HUD while a seek resolves). `phase` (ms) drives rotation. ----
pub struct Spinner {
    pub cx: f32,
    pub cy: f32,
    pub r: f32,
    pub phase: u32,
    pub col: [f32; 4],
    pub dots: usize,
    pub dot_r: f32,
}
impl Spinner {
    pub fn new(cx: f32, cy: f32, r: f32) -> Self {
        Self { cx, cy, r, phase: 0, col: [1.0, 1.0, 1.0, 1.0], dots: 10, dot_r: 3.4 }
    }
    pub fn phase(mut self, ms: u32) -> Self {
        self.phase = ms;
        self
    }
    pub fn tint(mut self, c: [f32; 4]) -> Self {
        self.col = c;
        self
    }
}
impl View for Spinner {
    fn draw(&self, _e: &Env, p: Painter) {
        const PERIOD: u32 = 760;
        let t = (self.phase % PERIOD) as f32 / PERIOD as f32;
        for i in 0..self.dots {
            let ang = i as f32 / self.dots as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let lead = (t - i as f32 / self.dots as f32).rem_euclid(1.0);
            let a = (1.0 - lead) * 0.85 + 0.12; // bright at the leading dot, fading behind it
            let c = [self.col[0], self.col[1], self.col[2], self.col[3] * a];
            let (dx, dy) = (self.cx + self.r * ang.cos(), self.cy + self.r * ang.sin());
            let d = self.dot_r;
            p.rect(Rect::new(dx - d, dy - d, 2.0 * d, 2.0 * d), d, c, c, 0.0);
        }
    }
}

// ---- TransportButton: circular control button with a runtime-rasterized SVG glyph
// (0 = subtitles/CC, 1 = audio). Focused = accent fill + dark icon; idle = faint fill + white
// icon. Mirrors the mockup's round icon buttons. ----
pub struct TransportButton {
    pub frame: Rect,
    pub which: i32,
    pub focused: bool,
}
impl TransportButton {
    pub fn new(which: i32, frame: Rect) -> Self {
        Self { frame, which, focused: false }
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
}
impl View for TransportButton {
    fn draw(&self, _e: &Env, p: Painter) {
        use crate::ui::icons::Icon;
        let r = self.frame;
        let (bg, ink) = if self.focused {
            (crate::ui::ACCENT, crate::ui::ACCENT_INK)
        } else {
            // solid clean dark disc (matches the icon mock ≈ #252525), so the white glyph reads the
            // same over any scene instead of a washed translucent circle
            ([0.145f32, 0.145, 0.153, 0.92], [1.0f32, 1.0, 1.0, 1.0])
        };
        p.rect(r, r.w * 0.5, bg, bg, 0.0); // circular
        let id = match self.which {
            1 => Icon::Audio,
            _ => Icon::Cc,
        };
        let s = (r.w * 0.54).round();
        let ir = Rect::new(r.x + (r.w - s) * 0.5, r.y + (r.h - s) * 0.5, s, s);
        crate::ui::icons::draw(p, id, ir, ink);
    }
}

// ---- TabPill: a rounded pill with a centered label. Focused = light pill + dark ink; idle =
// faint fill + dim ink. `pill_w(label, sz)` sizes it to fit. ----
pub struct TabPill {
    pub frame: Rect,
    pub label: *const c_char,
    pub sz: c_int,
    pub focused: bool,
}
impl TabPill {
    /// pill width for a `chars`-long label at `sz` (label advance + horizontal padding)
    pub fn width(chars: usize, sz: c_int) -> f32 {
        chars as f32 * sz as f32 * 0.56 + 44.0
    }
    pub fn new(label: *const c_char, sz: c_int, frame: Rect) -> Self {
        Self { frame, label, sz, focused: false }
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
}
impl View for TabPill {
    fn draw(&self, _e: &Env, p: Painter) {
        let r = self.frame;
        let (bg, ink) = if self.focused {
            ([0.95f32, 0.96, 0.98, 0.97], [0.07f32, 0.08, 0.10, 1.0])
        } else {
            ([1.0f32, 1.0, 1.0, 0.10], [0.84f32, 0.86, 0.92, 1.0])
        };
        p.rrect(r, r.h * 0.5, r.h * 0.5, bg);
        p.text(self.label, r.cx(), r.y + r.h * 0.5 - self.sz as f32 * 0.58, self.sz, ink, 1, 1);
    }
}

// ---- Button: a pill with a label and an optional leading icon, centered together as one group
// (icon + gap + label is centered in the pill). Focused = accent fill + dark ink; idle = faint
// fill + white. A reusable action button. ----
pub struct Button {
    pub frame: Rect,
    pub label: *const c_char,
    pub sz: c_int,
    pub icon: Option<crate::ui::icons::Icon>,
    pub focused: bool,
}
impl Button {
    pub fn new(label: *const c_char, sz: c_int, frame: Rect) -> Self {
        Self { frame, label, sz, icon: None, focused: false }
    }
    pub fn icon(mut self, i: crate::ui::icons::Icon) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
}
impl View for Button {
    fn draw(&self, _e: &Env, p: Painter) {
        let r = self.frame;
        let (bg, ink) = if self.focused {
            (crate::ui::ACCENT, crate::ui::ACCENT_INK)
        } else {
            ([1.0f32, 1.0, 1.0, 0.14], [1.0f32, 1.0, 1.0, 1.0])
        };
        p.rrect(r, r.h * 0.5, r.h * 0.5, bg);
        // center the [icon + gap + label] group in the pill
        let ty = r.y + r.h * 0.5 - self.sz as f32 * 0.58;
        let tw = crate::text::text_width(self.label, self.sz, 1);
        let (isz, gap) = if self.icon.is_some() { (self.sz as f32 * 1.15, 12.0) } else { (0.0, 0.0) };
        let gl = r.cx() - (isz + gap + tw) * 0.5;
        if let Some(icon) = self.icon {
            crate::ui::icons::draw(p, icon, Rect::new(gl, r.y + (r.h - isz) * 0.5, isz, isz), ink);
        }
        p.text(self.label, gl + isz + gap, ty, self.sz, ink, 0, 1); // left-aligned after the icon
    }
}

// ---- ProgressBar: the future player-HUD scrubber. Proves cross-screen reuse. ----
pub struct ProgressBar {
    pub frame: Rect,
    pub value: Spring,
    pub target: f32,
    pub buffered: f32,
    pub track: [f32; 4],
    pub fill: [f32; 4],
    pub knob: bool,
}
impl ProgressBar {
    pub fn new() -> Self {
        Self { frame: Rect::new(90.0, 890.0, 1740.0, 6.0), value: Spring::at(0.0), target: 0.0,
               buffered: 0.0, track: [1.0, 1.0, 1.0, 0.20], fill: [1.0, 1.0, 1.0, 0.95], knob: true }
    }
    pub fn set(&mut self, v: f32) {
        self.target = v.clamp(0.0, 1.0);
    }
}
impl View for ProgressBar {
    fn update(&mut self, env: &Env) {
        self.value.step(self.target, 220.0, env.dt);
    }
    fn layout(&mut self, f: Rect, _e: &Env) {
        self.frame = f;
    }
    fn draw(&self, _e: &Env, p: Painter) {
        let r = self.frame;
        let rad = r.h * 0.5;
        p.rrect(r, rad, rad, self.track);
        if self.buffered > 0.0 {
            p.rrect(Rect::new(r.x, r.y, r.w * self.buffered, r.h), rad, rad, [1.0, 1.0, 1.0, 0.28]);
        }
        let fw = (r.w * self.value.pos).max(r.h);
        p.rrect(Rect::new(r.x, r.y, fw, r.h), rad, rad, self.fill);
        if self.knob {
            let d = r.h * 2.6;
            let kx = r.x + r.w * self.value.pos - d * 0.5;
            p.rect(Rect::new(kx, r.y + r.h * 0.5 - d * 0.5, d, d), d * 0.5, [1.0; 4], [1.0; 4], 0.0);
        }
    }
}
