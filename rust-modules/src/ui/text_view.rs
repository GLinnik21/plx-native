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
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::os::raw::c_int;
use std::ptr::addr_of_mut;
use std::rc::Rc;

// Wrapping is pixel-measured (text_width per word), which is far too slow to redo every frame for
// every block — the text is stable, so memoize the wrapped lines by (text, sz, bold, width, lines).
// The lines are shared via Rc, so a cache hit is a refcount bump (not a Vec<String> deep-copy) each
// frame. Main-thread only (immediate-mode draw), like the poster/icon caches.
/// Wrapped lines plus whether `max_lines` cut the text off — the latter drives the optional
/// trailing run (a "MORE" affordance only makes sense when there is hidden text).
struct Wrapped {
    lines: Vec<String>,
    truncated: bool,
}
static mut WRAP_CACHE: Option<HashMap<u64, Rc<Wrapped>>> = None;

fn wrap_memo(key: u64, compute: impl FnOnce() -> Wrapped) -> Rc<Wrapped> {
    let cache = unsafe { (*addr_of_mut!(WRAP_CACHE)).get_or_insert_with(HashMap::new) };
    if let Some(v) = cache.get(&key) {
        return Rc::clone(v);
    }
    if cache.len() > 512 {
        cache.clear(); // crude cap — plenty for a page's worth of blocks across a few items
    }
    let v = Rc::new(compute());
    cache.insert(key, Rc::clone(&v));
    v
}

pub struct TextView<'a> {
    text: &'a str,
    sz: c_int,
    col: [f32; 4],
    bold: c_int,
    leading: f32,     // line pitch; 0 = derive from sz
    align: HAlign,
    max_lines: usize,                       // 0 = unlimited
    trailing: Option<(&'a str, [f32; 4])>,  // inline run after the last line when truncated (e.g. "MORE")
}

impl<'a> TextView<'a> {
    pub fn new(text: &'a str, sz: c_int, col: [f32; 4]) -> Self {
        Self { text, sz, col, bold: 0, leading: 0.0, align: HAlign::Left, max_lines: 0, trailing: None }
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
    /// An inline run (drawn bold in `col`) placed right after the last line — ONLY when the text was
    /// truncated by `max_lines`, i.e. there is hidden content. The classic "… MORE" affordance. It's
    /// positioned by the measured pixel width of the last line, so it hugs the text on left-aligned blocks.
    pub fn trailing(mut self, run: &'a str, col: [f32; 4]) -> Self {
        self.trailing = Some((run, col));
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
    fn wrap(&self, width: f32) -> Rc<Wrapped> {
        let mut h = DefaultHasher::new();
        self.text.hash(&mut h);
        self.sz.hash(&mut h);
        self.bold.hash(&mut h);
        (width as i32).hash(&mut h);
        self.max_lines.hash(&mut h);
        wrap_memo(h.finish(), || self.wrap_uncached(width))
    }

    /// greedy word-wrap to `width` px; the last line is ellipsized if `max_lines` truncates the text.
    fn wrap_uncached(&self, width: f32) -> Wrapped {
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
        let truncated = i < words.len(); // unplaced words remain → the block was cut off
        if truncated {
            // more text than fits — ellipsize the last placed line to width
            if let Some(last) = lines.last_mut() {
                *last = crate::text::elide(last, width, self.sz, self.bold, true);
            }
        }
        // safety: a lone token wider than the column can't be word-broken (a long URL/compound word,
        // or a space-less script that yields one "word") — ellipsize any over-wide line so it never
        // paints past the column (Painter has no clip). Also covers the whole-text-is-one-token case
        // that slips past the truncation gate above.
        for ln in lines.iter_mut() {
            if self.measure(ln) > width {
                *ln = crate::text::elide(ln, width, self.sz, self.bold, true);
            }
        }
        Wrapped { lines, truncated }
    }

    /// the height this occupies when wrapped to `width` (line count × pitch).
    pub fn measure_h(&self, width: f32) -> f32 {
        self.wrap(width).lines.len().max(1) as f32 * self.line_h()
    }

    /// Draw into `frame`: `frame.w` is the wrap width, `frame.x/y` the top-left. Line 0's cap band
    /// sits at `frame.y`, each subsequent line one `leading` below. Returns the consumed height.
    pub fn draw(&self, p: Painter, frame: Rect) -> f32 {
        let lh = self.line_h();
        let wrapped = self.wrap(frame.w);
        let lines = &wrapped.lines;
        let n = lines.len();
        // a trailing run ("MORE") paints only when max_lines actually hid words
        let run = self.trailing.filter(|_| wrapped.truncated);
        // reserve the run's (bold) width so the last line clips short and "… MORE" never spills the column
        let reserve = run
            .and_then(|(r, _)| CString::new(r).ok())
            .map(|c| crate::text::text_width(c.as_ptr(), self.sz, 1) + 16.0)
            .unwrap_or(0.0);
        for (i, ln) in lines.iter().enumerate() {
            let is_last = i + 1 == n;
            let text: Cow<str> = if is_last && reserve > 0.0 && self.measure(ln) + reserve > frame.w {
                Cow::Owned(crate::text::elide(ln, (frame.w - reserve).max(0.0), self.sz, self.bold, true))
            } else {
                Cow::Borrowed(ln.as_str())
            };
            let row = Rect::new(frame.x, frame.y + i as f32 * lh, frame.w, 0.0);
            if let Ok(cs) = CString::new(text.as_ref()) {
                let mut lab = Label::new(cs.as_ptr(), self.sz, self.col).h(self.align).v(VAlign::CapTop);
                if self.bold == 1 {
                    lab = lab.bold();
                }
                lab.draw(p, row);
            }
            // the run hugs the (possibly clipped) last line, positioned by its measured width
            if is_last {
                if let Some((r, rc)) = run {
                    if let Ok(cs) = CString::new(r) {
                        let rx = frame.x + self.measure(text.as_ref()) + 8.0;
                        Label::new(cs.as_ptr(), self.sz, rc).bold().h(HAlign::Left).v(VAlign::CapTop).draw(p, Rect::new(rx, row.y, frame.w, 0.0));
                    }
                }
            }
        }
        n as f32 * lh
    }
}

