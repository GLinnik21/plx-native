//! `TextView` — multi-line text laid out by cap band, pixel-wrapped to a width.
//!
//! The multi-line counterpart to [`Label`](crate::ui::label::Label): give it a raw string and a
//! wrap width and it word-wraps by **measured pixel width** (not a crude character count), stacks
//! the lines by a consistent leading with each line positioned by its cap band (layout ≠ paint —
//! descenders on the last line spill past the box and never enter the maths), truncates to an
//! optional line limit with an ellipsis, and reports the height it consumes so a flow layout can
//! stack the next block below it. This replaces the hand-rolled `wrap_*` helpers + repeated
//! `Painter::text` calls that used to live in every screen.
//!
//! Wrapping is recomputed on `draw`/`measure` (immediate-mode); the runs are short. If a hot path
//! ever needs it, cache by `(text, width)` — the API already isolates the wrap step.
use crate::ui::label::{HAlign, Label, VAlign};
use crate::ui::{Painter, Rect};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::os::raw::c_int;
use std::ptr::addr_of_mut;

// Wrapping is pixel-measured (text_width per word), which is far too slow to redo every frame for
// every block — the text is stable, so memoize the wrapped lines by (text, sz, bold, width, lines).
// Main-thread only (immediate-mode draw), like the poster/icon caches.
static mut WRAP_CACHE: Option<HashMap<u64, Vec<String>>> = None;

fn wrap_memo(key: u64, compute: impl FnOnce() -> Vec<String>) -> Vec<String> {
    let cache = unsafe { (*addr_of_mut!(WRAP_CACHE)).get_or_insert_with(HashMap::new) };
    if let Some(v) = cache.get(&key) {
        return v.clone();
    }
    if cache.len() > 512 {
        cache.clear(); // crude cap — plenty for a page's worth of blocks across a few items
    }
    let v = compute();
    cache.insert(key, v.clone());
    v
}

pub struct TextView<'a> {
    text: &'a str,
    sz: c_int,
    col: [f32; 4],
    bold: c_int,
    leading: f32,     // line pitch; 0 = derive from sz
    align: HAlign,
    max_lines: usize, // 0 = unlimited
}

impl<'a> TextView<'a> {
    pub fn new(text: &'a str, sz: c_int, col: [f32; 4]) -> Self {
        Self { text, sz, col, bold: 0, leading: 0.0, align: HAlign::Left, max_lines: 0 }
    }
    pub fn bold(mut self) -> Self {
        self.bold = 1;
        self
    }
    /// line pitch (cap-top to cap-top). Defaults to `sz * 1.32`.
    pub fn leading(mut self, px: f32) -> Self {
        self.leading = px;
        self
    }
    pub fn h(mut self, a: HAlign) -> Self {
        self.align = a;
        self
    }
    /// cap the block to `n` lines, ellipsizing the last (0 = unlimited).
    pub fn max_lines(mut self, n: usize) -> Self {
        self.max_lines = n;
        self
    }

    fn line_h(&self) -> f32 {
        if self.leading > 0.0 { self.leading } else { self.sz as f32 * 1.32 }
    }

    fn measure(&self, s: &str) -> f32 {
        CString::new(s)
            .ok()
            .map(|c| crate::text::text_width(c.as_ptr(), self.sz, self.bold))
            .unwrap_or(0.0)
    }

    /// wrapped lines for `width`, memoized (the pixel-wrap is too costly to redo every frame).
    fn wrap(&self, width: f32) -> Vec<String> {
        let mut h = DefaultHasher::new();
        self.text.hash(&mut h);
        self.sz.hash(&mut h);
        self.bold.hash(&mut h);
        (width as i32).hash(&mut h);
        self.max_lines.hash(&mut h);
        wrap_memo(h.finish(), || self.wrap_uncached(width))
    }

    /// greedy word-wrap to `width` px; the last line is ellipsized if `max_lines` truncates the text.
    fn wrap_uncached(&self, width: f32) -> Vec<String> {
        let words: Vec<&str> = self.text.split_whitespace().collect();
        let mut lines: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut i = 0;
        while i < words.len() {
            let trial = if cur.is_empty() { words[i].to_string() } else { format!("{cur} {}", words[i]) };
            if !cur.is_empty() && self.measure(&trial) > width {
                lines.push(std::mem::take(&mut cur));
                if self.max_lines > 0 && lines.len() == self.max_lines {
                    break; // out of line budget; `i` still points at unplaced words → truncated
                }
                cur = words[i].to_string();
            } else {
                cur = trial;
            }
            i += 1;
        }
        if !cur.is_empty() && (self.max_lines == 0 || lines.len() < self.max_lines) {
            lines.push(cur);
            i = words.len();
        }
        if i < words.len() {
            // more text than fits — ellipsize the last placed line to width
            if let Some(last) = lines.last_mut() {
                *last = ellipsize(last, width, self.sz, self.bold);
            }
        }
        lines
    }

    /// the height this occupies when wrapped to `width` (line count × pitch).
    pub fn measure_h(&self, width: f32) -> f32 {
        self.wrap(width).len().max(1) as f32 * self.line_h()
    }

    /// Draw into `frame`: `frame.w` is the wrap width, `frame.x/y` the top-left. Line 0's cap band
    /// sits at `frame.y`, each subsequent line one `leading` below. Returns the consumed height.
    pub fn draw(&self, p: Painter, frame: Rect) -> f32 {
        let lh = self.line_h();
        let lines = self.wrap(frame.w);
        for (i, ln) in lines.iter().enumerate() {
            let Ok(cs) = CString::new(ln.as_str()) else { continue };
            let row = Rect::new(frame.x, frame.y + i as f32 * lh, frame.w, 0.0);
            let mut lab = Label::new(cs.as_ptr(), self.sz, self.col).h(self.align).v(VAlign::CapTop);
            if self.bold == 1 {
                lab = lab.bold();
            }
            lab.draw(p, row);
        }
        lines.len() as f32 * lh
    }
}

/// truncate `s` to `"prefix…"` fitting `width` px (binary search on the char prefix).
fn ellipsize(s: &str, width: f32, sz: c_int, bold: c_int) -> String {
    let measure = |t: &str| CString::new(t).ok().map(|c| crate::text::text_width(c.as_ptr(), sz, bold)).unwrap_or(0.0);
    let full = format!("{s}\u{2026}");
    if measure(&full) <= width {
        return full;
    }
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cand: String = chars[..mid].iter().collect::<String>() + "\u{2026}";
        if measure(&cand) <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect::<String>() + "\u{2026}"
}
