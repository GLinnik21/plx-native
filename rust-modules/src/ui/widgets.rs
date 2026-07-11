//! Reusable retui leaves + shared helpers. These are the "reusable UI elements":
//! Button, CircleButton, PageDots, ProgressBar (the future player-HUD
//! scrubber), plus the poster-resolve + text-wrap helpers Card/Hero share.
#![allow(dead_code)]
use crate::pms::PmsMovie;
use crate::ui::theme;
use crate::ui::{Env, Painter, Rect, Size, Spring, View};
use std::os::raw::{c_char, c_int};

/// build the transcode key on the stack and resolve it to a GL texture (0 until loaded)
pub(crate) fn resolve_tex(path: *const c_char, w: c_int, h: c_int, png: c_int) -> u32 {
    let mut key = [0u8; 352];
    crate::posters::poster_key(key.as_mut_ptr() as *mut c_char, key.len(), path, w, h, png);
    crate::posters::poster_get(key.as_ptr() as *const c_char)
}

/// Source art for a [`card`]: a catalog poster (resolved 250×375, dark gradient skeleton) or any
/// keyed thumbnail at an explicit resolution (flat placeholder skeleton).
pub(crate) enum Art<'a> {
    Poster(Option<&'a PmsMovie>),
    Thumb { key: &'a str, res: (c_int, c_int) },
}

/// The one art-tile draw op. Resolves `art` to a texture (or a dark skeleton), draws it at `frame`
/// scaled about its centre when `focused`, then an optional focus ring `(pad, rad)` at focus=1.0.
/// Every art tile routes through here — the home grid posters (via [`draw_poster`]) and the
/// episode/chapters strips + detail Related (via [`draw_card`]) — so they resolve, skeleton, and
/// ring identically. (Home draws its own bigger glow ring with a custom focus scalar, so its shim
/// passes `ring: None`.)
pub(crate) fn card(p: Painter, frame: Rect, art: Art, rad: f32, focused: bool, scale: f32, ring: Option<(f32, f32)>) {
    let r = if focused { frame.scaled(scale) } else { frame };
    match art {
        Art::Poster(m) => {
            let t = m
                .filter(|m| m.thumb[0] != 0)
                .map(|m| resolve_tex(m.thumb.as_ptr() as *const c_char, 250, 375, 0))
                .unwrap_or(0);
            if t != 0 {
                p.tex(t, r, rad, theme::TINT_WHITE);
            } else {
                p.rect(r, rad, theme::SKELETON_TOP, theme::SKELETON_BOT, 0.0);
            }
        }
        Art::Thumb { key, res } => {
            let t = if key.is_empty() {
                0
            } else {
                std::ffi::CString::new(key).ok().map(|tp| resolve_tex(tp.as_ptr(), res.0, res.1, 0)).unwrap_or(0)
            };
            if t != 0 {
                p.tex(t, r, rad, theme::TINT_WHITE);
            } else {
                p.rrect(r, rad, rad, theme::CARD_PLACEHOLDER);
            }
        }
    }
    if let Some((pad, ring_rad)) = ring {
        if focused {
            p.ring(r, pad, ring_rad, 1.0);
        }
    }
}

/// a poster card (home grid): artwork if loaded, else a dark skeleton. The rect is already scaled by
/// the caller, which draws its own big glow ring, so no scale-pop/ring here.
pub(crate) fn draw_poster(p: Painter, m: Option<&PmsMovie>, r: Rect, rad: f32) {
    card(p, r, Art::Poster(m), rad, false, 1.0, None);
}

/// The scale a focused card pops to (shared by every animated card row).
pub(crate) const CARD_FOCUS_SCALE: f32 = 1.07;

/// A media card shared by the episode picker, the chapters strip, and the detail Related row so they
/// resolve + animate identically: the thumbnail (at `res`, or a dark placeholder), a focus scale-pop
/// about the centre + a tight focus ring when `focused` (the caller owns the `scale` spring).
pub(crate) fn draw_card(p: Painter, frame: Rect, thumb: &str, res: (c_int, c_int), radius: f32, focused: bool, scale: f32) {
    card(p, frame, Art::Thumb { key: thumb, res }, radius, focused, scale, Some((6.0, 14.0)));
}

/// read a NUL-terminated C-string field into a Rust String
pub(crate) fn cfield(b: &[u8]) -> String {
    let n = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).into_owned()
}

// ---- CircleButton: circular disc + centered glyph, same ControlStyle family as Button /
// TransportButton (focused = ACCENT, idle = solid dark disc). The hero + detail +/i/> circles. ----
pub struct CircleButton {
    pub frame: Rect,
    pub glyph: *const c_char,
    pub focused: bool,
    pub style: ControlStyle,
}
impl CircleButton {
    pub fn new(glyph: *const c_char) -> Self {
        Self { frame: Rect::new(0.0, 0.0, 60.0, 60.0), glyph, focused: false, style: ControlStyle::Accent }
    }
    pub fn at(mut self, x: f32, y: f32) -> Self {
        self.frame.x = x;
        self.frame.y = y;
        self
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
    pub fn style(mut self, s: ControlStyle) -> Self {
        self.style = s;
        self
    }
}
impl View for CircleButton {
    fn draw(&self, _e: &Env, p: Painter) {
        let r = self.frame;
        let (face, ink) = self.style.colors(self.focused);
        p.rect(r, r.w * 0.5, face, face, 0.0);
        // glyph centred on the disc by its cap band (layout ≠ paint), not a hand-tuned y
        crate::ui::label::Label::new(self.glyph, 32, ink)
            .h(crate::ui::label::HAlign::Center)
            .draw(p, r);
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
            (theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_INK)
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
/// How a `TabPill` reads. The player Info/Chapters tabs are always-filled buttons; the detail season
/// tabs are a segmented control with two *independent* states — a **selected** segment (the active
/// one, whose content shows) and a **highlighted** one (where the remote focus is).
#[derive(Clone, Copy)]
enum TabStyle {
    /// always a pill — focused → ACCENT, idle → solid dark disc (player Info/Chapters).
    Button,
    /// segmented control (detail season tabs): the focused segment is a bright ACCENT pill; the
    /// selected segment gets a subtle pill while focus is elsewhere; the rest are plain dim text.
    Segment { selected: bool },
}

// ---- TabPill: a rounded pill with a centered label, in one of two state models (TabStyle). ----
pub struct TabPill {
    pub frame: Rect,
    pub label: *const c_char,
    pub sz: c_int,
    pub focused: bool,
    style: TabStyle,
}
impl TabPill {
    /// pill width for a `chars`-long label at `sz` (label advance + horizontal padding)
    pub fn width(chars: usize, sz: c_int) -> f32 {
        chars as f32 * sz as f32 * 0.56 + 44.0
    }
    pub fn new(label: *const c_char, sz: c_int, frame: Rect) -> Self {
        Self { frame, label, sz, focused: false, style: TabStyle::Button }
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
    /// switch to the segmented-control look (detail season tabs); `selected` = the active segment.
    pub fn segment(mut self, selected: bool) -> Self {
        self.style = TabStyle::Segment { selected };
        self
    }
}
impl View for TabPill {
    fn draw(&self, _e: &Env, p: Painter) {
        let r = self.frame;
        // (fill, ink, bold): a highlighted (focused) tab is a bright ACCENT pill; a selected-but-
        // unfocused segment is a subtle pill; a plain segment is dim text with no pill at all.
        let (fill, ink, bold) = match self.style {
            TabStyle::Button if self.focused => (Some(crate::ui::ACCENT), crate::ui::ACCENT_INK, true),
            TabStyle::Button => (Some(theme::CONTROL_IDLE_FILL), theme::CONTROL_IDLE_INK, true),
            TabStyle::Segment { .. } if self.focused => (Some(crate::ui::ACCENT), crate::ui::ACCENT_INK, true),
            TabStyle::Segment { selected: true } => (Some(theme::OVERLAY_FOCUS_PILL), theme::TEXT_PRIMARY, true),
            TabStyle::Segment { .. } => (None, theme::TEXT_TERTIARY, false),
        };
        if let Some(bg) = fill {
            p.rrect(r, r.h * 0.5, r.h * 0.5, bg);
        }
        use crate::ui::label::{HAlign, Label};
        let mut lab = Label::new(self.label, self.sz, ink).h(HAlign::Center);
        if bold {
            lab = lab.bold();
        }
        lab.draw(p, r);
    }
}

/// Which colour treatment a control (Button / CircleButton) wears. One control widget, three looks —
/// so the hero Play CTA, the focus-driven detail/info buttons, and one-off cases all share code.
#[derive(Clone, Copy)]
pub enum ControlStyle {
    /// focus-driven: focused → warm ACCENT + dark ink; idle → solid dark disc + white ink. The
    /// default, and the shared look of the transport buttons / info-card actions / detail buttons.
    Accent,
    /// always the cool-white primary CTA (never darkens) — the hero Play button.
    Primary,
    /// caller supplies the exact fill + ink.
    Custom { fill: [f32; 4], ink: [f32; 4] },
}
impl ControlStyle {
    /// (fill, ink) for this style at the given focus state.
    pub(crate) fn colors(self, focused: bool) -> ([f32; 4], [f32; 4]) {
        match self {
            ControlStyle::Accent if focused => (crate::ui::ACCENT, crate::ui::ACCENT_INK),
            ControlStyle::Accent => (theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_INK),
            ControlStyle::Primary => (theme::FILL_PRIMARY, theme::INK_ON_PRIMARY),
            ControlStyle::Custom { fill, ink } => (fill, ink),
        }
    }
}

// ---- Button: a pill with a label and an optional leading icon, centered together as one group
// (icon + gap + label is centered in the pill). Colour per `ControlStyle` (default Accent). The one
// reusable action button — hero Play (Primary), detail/info actions (Accent), etc. ----
pub struct Button {
    pub frame: Rect,
    pub label: *const c_char,
    pub sz: c_int,
    pub icon: Option<crate::ui::icons::Icon>,
    pub focused: bool,
    pub style: ControlStyle,
}
impl Button {
    pub fn new(label: *const c_char, sz: c_int, frame: Rect) -> Self {
        Self { frame, label, sz, icon: None, focused: false, style: ControlStyle::Accent }
    }
    pub fn icon(mut self, i: crate::ui::icons::Icon) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
    pub fn style(mut self, s: ControlStyle) -> Self {
        self.style = s;
        self
    }
}
impl View for Button {
    fn draw(&self, _e: &Env, p: Painter) {
        let r = self.frame;
        let (bg, ink) = self.style.colors(self.focused);
        p.rrect(r, r.h * 0.5, r.h * 0.5, bg);
        // center the [icon + gap + label] group in the pill; the label sits on the pill centre by
        // its cap band, so descenders (the g's in "From Beginning") don't drag the caps upward
        let ty = crate::text::text_vcenter_y(self.sz, 1, r.y + r.h * 0.5);
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
               buffered: 0.0, track: theme::RAIL_TRACK, fill: theme::RAIL_FILL, knob: true }
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
            p.rrect(Rect::new(r.x, r.y, r.w * self.buffered, r.h), rad, rad, theme::RAIL_BUFFERED);
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
