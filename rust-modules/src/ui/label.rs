//! `Label` — text laid out by its **cap band**, painted as ink that is free to overflow.
//!
//! The one rule this encodes is **layout ≠ paint** (the UIKit frame-vs-drawn-content split, minus
//! the retained view tree we don't need). A label's *layout box* is the `frame` you draw it into and
//! the font's cap-top→baseline band inside it — a stable metric, identical for every string at a
//! size. Its *paint* is the actual glyph ink, which may spill past the box: descenders (g j y p q)
//! hang below the baseline, tall ascenders/diacritics rise above the cap. That overflow is purely
//! cosmetic and never enters any positioning maths, so "From Beginning" and "Go to Movie" align the
//! same way. Alignment is therefore string-independent — the thing hand-rolled `y - sz*0.58` guesses
//! kept getting subtly wrong.
//!
//! The same rule holds for the other things we paint outside a frame — the shader-baked focus
//! glow, the focus scale-pop (`Rect::scaled`), gradient scrims. They decorate; they are
//! never measured. Layout only ever sees frames and cap bands.
use crate::ui::{Painter, Rect};
use std::os::raw::{c_char, c_int};

/// Horizontal placement of the run within its frame.
#[derive(Clone, Copy)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

/// Vertical placement — always by the font cap band, so descenders never shift the run.
#[derive(Clone, Copy)]
pub enum VAlign {
    /// cap band centred on the frame's vertical centre (the usual case)
    Middle,
    /// cap-top sits on the frame's top edge
    CapTop,
    /// baseline sits on the frame's bottom edge
    Baseline,
}

/// A single run of text positioned by its cap band. Cheap + `Copy` (borrows the C string), built to
/// drop into the immediate-mode draw code. See the module docs for the layout ≠ paint rule.
#[derive(Clone, Copy)]
pub struct Label {
    text: *const c_char,
    sz: c_int,
    bold: c_int,
    col: [f32; 4],
    h: HAlign,
    v: VAlign,
}
impl Label {
    pub fn new(text: *const c_char, sz: c_int, col: [f32; 4]) -> Self {
        Self { text, sz, bold: 0, col, h: HAlign::Left, v: VAlign::Middle }
    }
    pub fn bold(mut self) -> Self {
        self.bold = 1;
        self
    }
    pub fn h(mut self, h: HAlign) -> Self {
        self.h = h;
        self
    }
    pub fn v(mut self, v: VAlign) -> Self {
        self.v = v;
        self
    }
    /// Draw into `frame` (the layout box). Horizontal per `h`; vertical per `v`, always off the cap
    /// band. Returns the painted width. The ink may extend outside `frame` — that is intended.
    pub fn draw(&self, p: Painter, frame: Rect) -> f32 {
        let (x, code) = match self.h {
            HAlign::Left => (frame.x, 0),
            HAlign::Center => (frame.cx(), 1),
            HAlign::Right => (frame.x + frame.w, 2),
        };
        let (cap_top, baseline) = crate::text::text_cap_band(self.sz, self.bold);
        let y = match self.v {
            VAlign::Middle => frame.y + frame.h * 0.5 - (cap_top + baseline) * 0.5,
            VAlign::CapTop => frame.y - cap_top,
            VAlign::Baseline => frame.y + frame.h - baseline,
        };
        p.text(self.text, x, y, self.sz, self.col, code, self.bold)
    }
}
