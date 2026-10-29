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
use crate::{Painter, Rect};
use std::os::raw::{c_char, c_int};

/// Horizontal placement of the run within its frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
    /// The edge the run's OWN reading direction begins at: the left for a Latin line, the right for
    /// an Arabic or Hebrew one ([`plx_gfx::text::is_rtl_paragraph`]). [`Label`] settles it against
    /// its text, [`TextView`](crate::text_view::TextView) reads its `Left` as this, and a screen
    /// that draws a line itself asks [`HAlign::resolve`]; a placement with no text to read takes it
    /// as `Left`.
    Start,
}

impl HAlign {
    /// `Start` settled against the line it places: `Right` when `text` reads right to left, else
    /// `Left`. `Left`, `Center` and `Right` are positions in the layout and come back unchanged.
    pub fn resolve(self, text: &str) -> HAlign {
        match self {
            HAlign::Start if plx_gfx::text::is_rtl_paragraph(text) => HAlign::Right,
            HAlign::Start => HAlign::Left,
            at => at,
        }
    }

    /// Where a run's anchor stands in `frame`, with the [`Painter::text`] align code that hangs the
    /// run there. An unresolved `Start` reads as `Left`.
    pub fn anchor(self, frame: Rect) -> (f32, c_int) {
        match self {
            HAlign::Left | HAlign::Start => (frame.x, 0),
            HAlign::Center => (frame.cx(), 1),
            HAlign::Right => (frame.x + frame.w, 2),
        }
    }
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
        Self {
            text,
            sz,
            bold: 0,
            col,
            h: HAlign::Left,
            v: VAlign::Middle,
        }
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
    /// The run's text, read only to settle a [`HAlign::Start`].
    fn text_lossy(&self) -> std::borrow::Cow<'_, str> {
        if self.text.is_null() {
            return "".into();
        }
        // SAFETY: the pointer is a live NUL-terminated string for the draw: the contract `Painter::text`
        // reads it under too (ui/CLAUDE.md's first gotcha).
        unsafe { std::ffi::CStr::from_ptr(self.text) }.to_string_lossy()
    }
    /// Draw into `frame` (the layout box). Horizontal per `h`; vertical per `v`, always off the cap
    /// band. Returns the painted width. The ink may extend outside `frame` — that is intended.
    pub fn draw(&self, p: Painter, frame: Rect) -> f32 {
        let h = match self.h {
            HAlign::Start => HAlign::Start.resolve(&self.text_lossy()),
            at => at,
        };
        let (x, code) = h.anchor(frame);
        let (cap_top, baseline) = plx_gfx::text::text_cap_band(self.sz, self.bold);
        let y = match self.v {
            VAlign::Middle => frame.y + frame.h * 0.5 - (cap_top + baseline) * 0.5,
            VAlign::CapTop => frame.y - cap_top,
            VAlign::Baseline => frame.y + frame.h - baseline,
        };
        p.text(self.text, x, y, self.sz, self.col, code, self.bold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Start` is the edge a run's OWN reading direction begins at: the right edge for an Arabic or
    /// Hebrew line, the left for Latin and for text with no letter to decide. `Left`, `Center` and
    /// `Right` are positions in the layout, and a direction never moves them.
    #[test]
    fn start_resolves_to_the_edge_the_lines_own_direction_begins_at() {
        for (text, want) in [
            ("عرض جديد على Netflix", HAlign::Right),
            ("שלום עולם", HAlign::Right),
            ("Netflix عرض جديد", HAlign::Left),
            ("1984", HAlign::Left),
            ("", HAlign::Left),
        ] {
            assert_eq!(HAlign::Start.resolve(text), want, "{text:?}");
        }
        for at in [HAlign::Left, HAlign::Center, HAlign::Right] {
            assert_eq!(at.resolve("عرض جديد"), at);
        }
    }

    /// A `Start` label is drawn flush with ITS start edge: the frame's right edge for Arabic, its
    /// left for Latin. A plain `Left` label stays left whatever it says.
    #[test]
    fn a_start_label_is_drawn_flush_with_its_start_edge() {
        let _serial = plx_base::testlock::serial();
        let frame = Rect::new(30.0, 10.0, 120.0, 20.0);
        for (text, at, want) in [
            (c"عرض جديد", HAlign::Start, (2, frame.x + frame.w)),
            (c"Netflix", HAlign::Start, (0, frame.x)),
            (c"عرض جديد", HAlign::Left, (0, frame.x)),
        ] {
            let runs = crate::draw_census::capture_runs(|| {
                Label::new(text.as_ptr(), 20, [1.0; 4]).h(at).draw(Painter::recording(), frame);
            });
            assert_eq!(runs.len(), 1, "{runs:?}");
            assert_eq!((runs[0].align, runs[0].at.x), want, "{text:?} {at:?}");
        }
    }
}
