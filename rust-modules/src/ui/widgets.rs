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
pub(crate) fn draw_poster(p: Painter, m: *const PmsMovie, r: Rect, rad: f32) {
    const SK_T: [f32; 4] = [0.13, 0.14, 0.17, 1.0];
    const SK_B: [f32; 4] = [0.08, 0.09, 0.11, 1.0];
    unsafe {
        if !m.is_null() && (*m).thumb[0] != 0 {
            let t = resolve_tex((*m).thumb.as_ptr() as *const c_char, 250, 375, 0);
            if t != 0 {
                p.tex(t, r, rad, [1.0; 4]);
                return;
            }
        }
    }
    p.rect(r, rad, SK_T, SK_B, 0.0);
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
