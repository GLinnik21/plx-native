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
use std::rc::Rc;

// Wrapping is pixel-measured (text_width per word), which is far too slow to redo every frame for
// every block — the text is stable, so memoize the wrapped lines by (text, sz, bold, width, lines).
// The lines are shared via Rc, so a cache hit is a refcount bump. Main-thread only (immediate-mode
// draw), like the poster/icon caches.
/// Wrapped lines plus whether `max_lines` cut the text off — the latter drives the optional
/// trailing run (a "MORE" affordance only makes sense when there is hidden text).
///
/// Lines are stored **NUL-terminated (`CString`), built once at wrap time**: draw hands
/// `as_ptr()` straight to the text backend, so a cache hit paints the whole block with ZERO
/// per-frame allocation. (It used to store `String` and re-run `CString::new` per line per
/// frame — on the detail episode strip that was ~40 allocations a frame, against the UI's
/// explicit zero-alloc goal.)
struct Wrapped {
    lines: Vec<CString>,
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
    /// Inline BOLD run before the first word (e.g. the detail hero's `S2, E3 · Laura:` episode
    /// prefix). The mirror of [`TextView::trailing`], and it has to be part of the view rather than a
    /// separately-drawn label because the wrap has to know about it: line 0's available width is
    /// reduced by the run, every later line gets the full column. Drawn bold in its own colour.
    lead: Option<(&'a str, [f32; 4])>,
    /// is the lead run drawn BOLD (`lead`) or at the block's own weight (`lead_quiet`)
    lead_bold: bool,
    fade_last: f32, // px reserved at the wrap width's right edge; >0 fades a truncated last line out before it
}

impl<'a> TextView<'a> {
    pub fn new(text: &'a str, sz: c_int, col: [f32; 4]) -> Self {
        Self { text, sz, col, bold: 0, leading: 0.0, align: HAlign::Left, max_lines: 0, trailing: None, lead: None, lead_bold: true, fade_last: 0.0 }
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
    ///
    /// **The app's production truncation mark is [`fade_last`](Self::fade_last)'s**, not this one —
    /// both the About card and the person page's bio pin a quiet MORE to their column's right edge
    /// and dissolve the last line into it. This stays because it is the right answer where there is
    /// no right edge to pin to (a centred or narrow block), and it is EXCLUSIVE with that one: see
    /// `fade_last` for why the two cannot both be set.
    pub fn trailing(mut self, run: &'a str, col: [f32; 4]) -> Self {
        self.trailing = Some((run, col));
        self.fade_last = 0.0; // exclusive — see `fade_last`
        self
    }
    /// Inline BOLD run before the first word, in its own colour — see [`TextView::lead`] the field.
    pub fn lead(mut self, run: &'a str, col: [f32; 4]) -> Self {
        self.lead = Some((run, col));
        self.lead_bold = true;
        self
    }
    /// [`TextView::lead`] at the block's OWN weight — a lead that differs from its text only in
    /// colour, which is what a label word before its value is ("Starring" before the names,
    /// `Details Screen.dc.html`'s people column). Bolding it there would make the quiet word the
    /// loudest thing in the block, which is the opposite of what dimming it is for.
    pub fn lead_quiet(mut self, run: &'a str, col: [f32; 4]) -> Self {
        self.lead = Some((run, col));
        self.lead_bold = false;
        self
    }
    /// The bold lead run's pixel width plus the space after it, or 0 with no lead. Line 0 wraps into
    /// `width - this`; every later line gets the whole column.
    fn lead_w(&self) -> f32 {
        self.lead
            .and_then(|(r, _)| CString::new(r).ok())
            .map(|c| crate::text::text_width(c.as_ptr(), self.sz, i32::from(self.lead_bold)) + self.measure(" "))
            .unwrap_or(0.0)
    }
    /// Reserve `px` at the wrap width's right edge for an OUT-OF-FLOW affordance sitting on the last
    /// line (the About card's right-pinned MORE): when the text was truncated by `max_lines` AND the
    /// last line reaches into that zone, the line paints with a shader fade to transparency ending at
    /// `width − px` instead of colliding. A no-op on non-truncated text. Left-aligned blocks only.
    ///
    /// **Setting this CLEARS [`trailing`](Self::trailing), and vice versa — the two are one choice,
    /// not two flags.** They are mutually exclusive in `draw` by construction: the fade branch ends
    /// in `continue`, which skips the inline-run block below it, so a view carrying both loses its
    /// MORE on exactly the lines that fade — i.e. on exactly the lines the affordance exists for,
    /// and nowhere a test of a short string would ever see it. Last builder call wins (the same rule
    /// `lead`/`lead_quiet` already follow), so the choice is made where the view is built.
    pub fn fade_last(mut self, px: f32) -> Self {
        self.fade_last = px;
        self.trailing = None; // exclusive — see above
        self
    }

    /// The px the truncated last line dissolves across, ending at the left edge of the zone
    /// [`fade_last`](Self::fade_last) reserved.
    ///
    /// **Derived from the type size, never a literal.** A dissolve reads by how many GLYPHS it
    /// spans, which is a property of the ink and not of the column it sits in — so a band tuned at
    /// one rung is wrong at another for the same reason a hand-tuned font size is. It was a bare
    /// `150.0` inside `draw`, tuned against the About card's 5 CAPTION lines in a 580px column; the
    /// person page's bio is a rung larger in a column twice as wide, and the same literal there
    /// would have dissolved over visibly fewer letters. 6.25 em reproduces that tuning exactly at
    /// `size::CAPTION` (24 × 6.25 = 150), so the card is byte-identical and every other rung now
    /// gets the same run of letters rather than the same run of pixels.
    fn fade_band(&self) -> f32 {
        const FADE_BAND_EM: f32 = 6.25;
        self.sz as f32 * FADE_BAND_EM
    }

    fn line_h(&self) -> f32 {
        if self.leading > 0.0 { self.leading } else { self.sz as f32 * 1.32 }
    }

    /// pixel width of an arbitrary `&str` — allocates a scratch CString, so WRAP-TIME ONLY
    /// (the trial strings). The draw path measures cached lines via [`measure_c`](Self::measure_c).
    fn measure(&self, s: &str) -> f32 {
        CString::new(s)
            .ok()
            .map(|c| crate::text::text_width(c.as_ptr(), self.sz, self.bold))
            .unwrap_or(0.0)
    }
    /// pixel width of an already-NUL-terminated cached line — no allocation, draw-path safe.
    fn measure_c(&self, c: &CString) -> f32 {
        crate::text::text_width(c.as_ptr(), self.sz, self.bold)
    }

    /// wrapped lines for `width`, memoized (the pixel-wrap is too costly to redo every frame).
    fn wrap(&self, width: f32) -> Rc<Wrapped> {
        let mut h = DefaultHasher::new();
        self.text.hash(&mut h);
        self.sz.hash(&mut h);
        self.bold.hash(&mut h);
        (width as i32).hash(&mut h);
        self.max_lines.hash(&mut h);
        // the lead run narrows line 0, so two views differing only in it wrap differently
        self.lead.map(|(r, _)| r).unwrap_or("").hash(&mut h);
        wrap_memo(h.finish(), || self.wrap_uncached(width))
    }

    /// greedy word-wrap to `width` px; the last line is ellipsized if `max_lines` truncates the text.
    fn wrap_uncached(&self, width: f32) -> Wrapped {
        // Split on whitespace EXCEPT U+00A0. A no-break space is the typographic instruction
        // "these two words are one atom", and `split_whitespace` honours the Unicode White_Space
        // property, which includes it — so the standard glue character silently did nothing.
        // `detail`'s people column is the case: the atoms of "Starring A B, C D" are the PEOPLE,
        // and a break inside one puts a surname alone on the next line.
        let words: Vec<&str> =
            self.text.split(|c: char| c.is_whitespace() && c != '\u{a0}').filter(|w| !w.is_empty()).collect();
        let mut lines: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut i = 0;
        // line 0 shares its row with the bold lead run, so it wraps into the column MINUS that run;
        // every later line gets the whole column back
        let lead_w = self.lead_w();
        let avail = |line: usize| if line == 0 { (width - lead_w).max(0.0) } else { width };
        while i < words.len() {
            let trial = if cur.is_empty() { words[i].to_string() } else { format!("{cur} {}", words[i]) };
            if !cur.is_empty() && self.measure(&trial) > avail(lines.len()) {
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
            let li = lines.len().saturating_sub(1);
            let w = if li == 0 { (width - lead_w).max(0.0) } else { width };
            if let Some(last) = lines.last_mut() {
                *last = crate::text::elide(last, w, self.sz, self.bold, true);
            }
        }
        // safety: a lone token wider than the column can't be word-broken (a long URL/compound word,
        // or a space-less script that yields one "word") — ellipsize any over-wide line so it never
        // paints past the column (Painter has no clip). Also covers the whole-text-is-one-token case
        // that slips past the truncation gate above.
        for (li, ln) in lines.iter_mut().enumerate() {
            let w = if li == 0 { (width - lead_w).max(0.0) } else { width };
            if self.measure(ln) > w {
                *ln = crate::text::elide(ln, w, self.sz, self.bold, true);
            }
        }
        // NUL-terminate once, here — every later frame draws these by pointer (interior NULs
        // can't occur in PMS strings; degrade to an empty line rather than panic if one does)
        let lines = lines.into_iter().map(|s| CString::new(s).unwrap_or_default()).collect();
        Wrapped { lines, truncated }
    }

    /// the height this occupies when wrapped to `width` (line count × pitch).
    pub fn measure_h(&self, width: f32) -> f32 {
        self.wrap(width).lines.len().max(1) as f32 * self.line_h()
    }

    /// whether wrapping to `width` under the current `max_lines` hides any text — i.e. there is more
    /// to read. Drives an out-of-flow "MORE" affordance when the inline [`trailing`](Self::trailing)
    /// run isn't wanted (e.g. a corner label). Shares the memoised wrap, so it's free next to a draw.
    pub fn truncates(&self, width: f32) -> bool {
        self.wrap(width).truncated
    }

    /// The **cap-top y of the block's LAST line** — where an out-of-flow affordance pinned beside
    /// the fade ([`fade_last`](Self::fade_last)'s right-pinned MORE) has to sit — given the `top`
    /// the block was drawn at and the height [`draw`](Self::draw) returned for it.
    ///
    /// It lives here rather than at the two call sites because the only thing it can be derived
    /// from is `draw`'s own stacking rule (line `i`'s cap band at `top + i × line_h`, `n × line_h`
    /// returned), and that rule is this type's. Written out by a caller it becomes `top + h −
    /// <the leading I think I passed>` — a second copy of the pitch, one edit away from the mark
    /// floating half a line off the prose it belongs to. Both screens that pin a MORE now ask the
    /// view that drew the text.
    pub fn last_line_cap_y(&self, top: f32, drawn_h: f32) -> f32 {
        top + drawn_h - self.line_h()
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
        // fade_last: the last line dissolves to nothing across this band ending at the wrap width
        // minus the reserved affordance gap. Loop-invariant, so hoisted out.
        let fade_to = frame.w - self.fade_last;
        let fade_from = fade_to - self.fade_band();
        // the bold lead run sits at the column's left edge on line 0; line 0's text starts after it
        let lead_w = self.lead_w();
        if let Some((r, lc)) = self.lead {
            if let Ok(cs) = CString::new(r) {
                // A RIGHT-aligned block puts the lead immediately left of line 0's text, not at
                // the column's left edge: line 0 hugs the right margin, so anchoring the lead to
                // the far edge would strand the label word across the whole slack of the line
                // with its own value nowhere near it. Left/centre keep the column edge.
                let lx = if self.align == HAlign::Right {
                    let w0 = lines.first().map(|l| self.measure_c(l)).unwrap_or(0.0);
                    frame.x + frame.w - w0 - lead_w
                } else {
                    frame.x
                };
                let mut lab = Label::new(cs.as_ptr(), self.sz, lc).h(HAlign::Left).v(VAlign::CapTop);
                if self.lead_bold {
                    lab = lab.bold();
                }
                lab.draw(p, Rect::new(lx, frame.y, frame.w, 0.0));
            }
        }
        for (i, ln) in lines.iter().enumerate() {
            let is_last = i + 1 == n;
            let dx = if i == 0 { lead_w } else { 0.0 };
            // Clip the last line short of a reserved trailing run — RARE (a trailing affordance
            // on a truncated block, e.g. the About card) and the one owned CString on this path;
            // every ordinary line draws the CACHED CString by pointer, zero alloc.
            let clipped: Option<CString> = if is_last && reserve > 0.0 && self.measure_c(ln) + reserve > frame.w {
                let s = ln.to_str().unwrap_or("");
                CString::new(crate::text::elide(s, (frame.w - reserve).max(0.0), self.sz, self.bold, true)).ok()
            } else {
                None
            };
            let tc: &CString = clipped.as_ref().unwrap_or(ln);
            let row = Rect::new(frame.x + dx, frame.y + i as f32 * lh, frame.w - dx, 0.0);
            // a truncated last line with a fade_last reservation dissolves into the affordance zone
            // instead of colliding with it (only when it actually reaches that far)
            if is_last && wrapped.truncated && self.fade_last > 0.0 && self.measure_c(tc) > fade_from {
                let (ct, _) = crate::text::text_cap_band(self.sz, self.bold);
                // cap band at row.y, like Label's VAlign::CapTop
                p.text_fade(tc.as_ptr(), row.x, row.y - ct, self.sz, self.col, self.bold,
                    fade_from, fade_to);
                continue;
            }
            let mut lab = Label::new(tc.as_ptr(), self.sz, self.col).h(self.align).v(VAlign::CapTop);
            if self.bold == 1 {
                lab = lab.bold();
            }
            lab.draw(p, row);
            // the run hugs the (possibly clipped) last line, positioned by its measured width
            if is_last {
                if let Some((r, rc)) = run {
                    if let Ok(cs) = CString::new(r) {
                        let rx = frame.x + self.measure_c(tc) + 8.0;
                        Label::new(cs.as_ptr(), self.sz, rc).bold().h(HAlign::Left).v(VAlign::CapTop).draw(p, Rect::new(rx, row.y, frame.w, 0.0));
                    }
                }
            }
        }
        n as f32 * lh
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme;

    /// **The two truncation affordances are ONE choice, and the builder is what enforces it.**
    /// `draw`'s fade branch ends in `continue`, so a view carrying both loses its inline run on
    /// exactly the truncated lines that fade — the mark disappears where it is needed and nowhere
    /// else, which no test that draws a string short enough to fit could ever see. Whichever call
    /// came last is the one that survives, in both orders.
    #[test]
    fn the_two_truncation_affordances_cannot_be_stacked() {
        let faded = TextView::new("x", theme::size::BODY, theme::TEXT_PRIMARY)
            .trailing("MORE", theme::TEXT_PRIMARY)
            .fade_last(120.0);
        assert!(faded.trailing.is_none(), "the inline run survived a fade reservation and would be skipped");
        assert_eq!(faded.fade_last, 120.0);

        let inline = TextView::new("x", theme::size::BODY, theme::TEXT_PRIMARY)
            .fade_last(120.0)
            .trailing("MORE", theme::TEXT_PRIMARY);
        assert!(inline.trailing.is_some());
        assert_eq!(inline.fade_last, 0.0, "the fade reservation survived and would eat the inline run");
    }

    /// **A pinned MORE rides the line that faded under it**, whichever way the block set its pitch.
    /// The affordance is drawn by two pieces of code that never meet — this type dissolves the last
    /// line into a zone it reserved, the SCREEN pins the word beside that zone — so the only thing
    /// keeping them on one line is `last_line_cap_y` answering off the same `line_h` the stacking
    /// used. The explicit-`leading` case is the person page's bio and the About card; the DERIVED
    /// case (`sz × 1.32`) is the one a caller gets by forgetting to set a pitch at all, and it is
    /// what a hand-written `top + h − <a literal>` at a call site would silently get wrong.
    #[test]
    fn a_pinned_mark_sits_on_the_cap_band_of_the_line_that_faded() {
        let top = 137.0; // an arbitrary block offset — a measured flow's y is never round
        // Sub-px, not exact: the derived pitch is irrational in f32 (`28 × 1.32`), so `n·lh − lh`
        // and `(n−1)·lh` differ in the last mantissa bit. A tenth of a pixel is far tighter than
        // the half-leading drift this exists to catch, and the draw snaps to whole pixels anyway.
        let close = |a: f32, b: f32| (a - b).abs() < 0.1;
        for lines in 1..=5 {
            let n = lines as f32;
            let led = TextView::new("x", theme::size::BODY, theme::TEXT_PRIMARY).leading(40.0);
            let (got, want) = (led.last_line_cap_y(top, n * 40.0), top + (n - 1.0) * 40.0);
            assert!(
                close(got, want),
                "explicit leading: {lines} line(s) put the mark at {got}, not {want}"
            );
            let derived = TextView::new("x", theme::size::BODY, theme::TEXT_PRIMARY);
            let lh = theme::size::BODY as f32 * 1.32;
            let (got, want) = (derived.last_line_cap_y(top, n * lh), top + (n - 1.0) * lh);
            assert!(
                close(got, want),
                "derived leading: {lines} line(s) put the mark at {got}, not {want}"
            );
        }
    }

    /// The dissolve is measured in EM, so it spans the same run of LETTERS at every rung — and the
    /// About card, the one tuning this number ever had, must still come out to the pixel it did.
    #[test]
    fn the_fade_band_is_ink_relative_and_reproduces_the_about_card() {
        let band = |sz| TextView::new("x", sz, theme::TEXT_PRIMARY).fade_band();
        assert_eq!(band(theme::size::CAPTION), 150.0, "detail's About card must fade exactly as it did");
        assert!(band(theme::size::BODY) > band(theme::size::CAPTION), "a larger rung must dissolve over more px");
        // proportional, not merely monotonic — `DISPLAY` 48 is exactly twice `CAPTION` 24
        assert_eq!(band(theme::size::DISPLAY), 2.0 * band(theme::size::CAPTION));
    }
}

