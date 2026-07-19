//! Reusable retui leaves + shared helpers. These are the "reusable UI elements":
//! Button, CircleButton, TabPill, TransportButton, PageDots, Badge, plus the shared art-card
//! core (`card`/`draw_card`) and the poster-resolve helper. (Multi-line text wrapping
//! now lives in the `TextView` primitive in `text_view.rs`.)
use crate::pms::PmsMovie;
use crate::ui::theme;
use crate::ui::{Env, Painter, Rect, View};
use std::os::raw::{c_char, c_int};

/// build the transcode key on the stack and resolve it to a GL texture (0 until loaded).
/// Per-frame hot path (every visible tile) — the NUL-terminated copy poster_key wants is
/// made on the stack, no heap alloc.
pub(crate) fn resolve_tex(path: &str, w: c_int, h: c_int, png: c_int) -> u32 {
    if path.is_empty() {
        return 0;
    }
    let mut p = [0u8; 256];
    crate::cbuf::set_bytes(&mut p, path);
    let mut key = [0u8; 352];
    crate::posters::poster_key(key.as_mut_ptr() as *mut c_char, key.len(), p.as_ptr() as *const c_char, w, h, png);
    crate::posters::poster_get(key.as_ptr() as *const c_char)
}

/// Source art for a [`card`]: a catalog poster (resolved 250×375, dark gradient skeleton) or any
/// keyed thumbnail at an explicit resolution (flat placeholder skeleton).
pub(crate) enum Art<'a> {
    Poster(Option<&'a PmsMovie>),
    Thumb { key: &'a str, res: (c_int, c_int) },
}

/// The one art-tile draw op. Resolves `art` to a texture (or a dark skeleton) and draws it at `frame`,
/// scaled about its centre when `focused`. A textured tile routes through the CARD COMPOSITE
/// ([`Painter::tex_carded`]): texture + 1px edge-sheen + the soft drop-shadow that GROWS with the pop
/// factor `f` (0 = resting/close to the shelf, 1 = fully lifted), all in ONE pass. The caller supplies
/// `f` (the shelves compute it from their per-cell spring; the episode/chapters strips from `scale`).
/// A not-yet-loaded skeleton falls back to a rimmed fill (no shadow until the art arrives).
pub(crate) fn card(p: Painter, frame: Rect, art: Art, rad: f32, focused: bool, scale: f32, f: f32) {
    let r = if focused { frame.scaled(scale) } else { frame };
    match art {
        Art::Poster(m) => {
            let t = m.map(|m| resolve_tex(&m.thumb, 250, 375, 0)).unwrap_or(0);
            if t != 0 {
                p.tex_carded(t, r, rad, theme::TINT_WHITE, f);
            } else {
                p.rect_sheened(r, rad, theme::SKELETON_TOP, theme::SKELETON_BOT);
            }
            // The ONE progress language on every poster, drawn in this shared composite so Home
            // shelves + the Library grid + Related all inherit it: amber corner ANGLE = fully
            // unwatched; the amber resume BAR (card_row::resume_bar) = in progress. Never both —
            // an in-progress item isn't unwatched (`resume_ms == 0` gate).
            if let Some(m) = m {
                if m.unwatched && m.resume_ms == 0 {
                    // quantize to 4px so the focus-pop animation reuses ~6 cached mask
                    // textures instead of rasterizing+uploading one per rounded pixel
                    let d = ((r.w * 0.176) / 4.0).round() * 4.0; // ≈44px on a 250-wide card
                    crate::ui::icons::draw(
                        p,
                        crate::ui::icons::Icon::UnwatchedAngle,
                        Rect::new(r.x + r.w - d, r.y, d, d),
                        theme::RESUME_FILL,
                    );
                }
            }
        }
        Art::Thumb { key, res } => {
            let t = resolve_tex(key, res.0, res.1, 0);
            if t != 0 {
                p.tex_carded(t, r, rad, theme::TINT_WHITE, f);
            } else {
                p.rrect_sheened(r, rad, theme::CARD_PLACEHOLDER);
            }
        }
    }
}

/// The scale a focused card pops to (shared by every animated card row).
pub(crate) const CARD_FOCUS_SCALE: f32 = 1.07;

/// Icon box as a fraction of a round control's diameter — the ONE ratio every disc glyph uses
/// (transport CC/Audio buttons, CircleButton vector icons, the Continue-Watching play badge).
pub(crate) const DISC_ICON_RATIO: f32 = 0.54;

/// A media card shared by the episode picker and the chapters strip so they resolve + animate
/// identically: the thumbnail (at `res`, or a dark placeholder), a focus scale-pop about the centre +
/// the focus treatment (soft drop-shadow + top sheen) when `focused` (the caller owns the `scale`
/// spring).
pub(crate) fn draw_card(p: Painter, frame: Rect, thumb: &str, res: (c_int, c_int), radius: f32, focused: bool, scale: f32) {
    // pop factor from the caller's scale spring (0 at rest → 1 at full focus scale) drives the folded shadow
    let f = if focused { ((scale - 1.0) / (CARD_FOCUS_SCALE - 1.0)).clamp(0.0, 1.0) } else { 0.0 };
    card(p, frame, Art::Thumb { key: thumb, res }, radius, focused, scale, f);
}

/// The top-left profile chip's VISUAL (avatar texture, or an initial / person-glyph fallback,
/// with the shared tile shadow + sheen). Shared by Home and the Library screen — each screen owns
/// its rect + focus rule and calls this. The session lookup (mutex + UserRef clone) is
/// snapshotted per profile GENERATION, not per frame.
pub(crate) fn profile_chip(p: Painter, r: Rect, focused: bool) {
    use std::ffi::CString;
    use std::ptr::addr_of_mut;
    static mut CHIP: Option<(u32, String, CString)> = None; // (gen, thumb path, initial)
    let d = r.w;
    let gen = crate::plex::session::current_gen();
    let chip = unsafe { &mut *addr_of_mut!(CHIP) };
    if chip.as_ref().map(|c| c.0 != gen).unwrap_or(true) {
        let cur = crate::plex::session::current();
        let thumb = cur.as_ref().map(|u| u.thumb.clone()).unwrap_or_default();
        let initial = cur
            .as_ref()
            .and_then(|u| u.title.chars().next())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        *chip = Some((gen, thumb, CString::new(initial).unwrap_or_default()));
    }
    let (_, thumb_s, initial_c) = chip.as_ref().unwrap();
    // resting shadow + perimeter stroke always; lift the shadow when focused (same as shelf tiles)
    p.focus_shadow(r, d * 0.5, if focused { 1.0 } else { 0.0 });
    let mut drew = false;
    if !thumb_s.is_empty() {
        let t = resolve_tex(thumb_s, 128, 128, 0);
        if t != 0 {
            p.tex_stroked(t, r, d * 0.5, theme::TINT_WHITE);
            drew = true;
        }
    }
    if !drew {
        p.rect_sheened(r, d * 0.5, theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_FILL);
        if initial_c.as_bytes().is_empty() {
            // signed out (no session) — a generic person glyph; the menu behind it offers Sign in
            crate::ui::icons::draw(p, crate::ui::icons::Icon::User, r.inset(14.0), theme::TEXT_SECONDARY);
        } else {
            let ty = crate::text::text_vcenter_y(theme::size::HEADLINE, 1, r.y + d * 0.5);
            p.text(initial_c.as_ptr(), r.x + d * 0.5, ty, theme::size::HEADLINE, theme::TEXT_PRIMARY, 1, 1);
        }
    }
}

// ---- CircleButton: circular disc + centered glyph, same ControlStyle family as Button /
// TransportButton (focused = ACCENT, idle = solid dark disc). The hero + detail +/i/> circles. ----
pub struct CircleButton {
    pub frame: Rect,
    pub glyph: *const c_char,
    pub icon: Option<crate::ui::icons::Icon>, // vector glyph; overrides the text glyph when set
    pub focused: bool,
    pub style: ControlStyle,
}
impl CircleButton {
    pub fn new(glyph: *const c_char) -> Self {
        Self { frame: Rect::new(0.0, 0.0, 60.0, 60.0), glyph, icon: None, focused: false, style: ControlStyle::Accent }
    }
    pub fn at(mut self, x: f32, y: f32) -> Self {
        self.frame.x = x;
        self.frame.y = y;
        self
    }
    /// Render a vector icon centred on the disc instead of the text glyph (e.g. a real
    /// chevron rather than a ">" character). Pass a bare-stroke icon — one that carries its
    /// own outline circle (Info) would double-ring against the disc face.
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
impl View for CircleButton {
    fn draw(&self, _e: &Env, p: Painter) {
        let r = self.frame;
        let (face, ink) = self.style.colors(self.focused);
        p.rect(r, r.w * 0.5, face, face, 0.0);
        if let Some(icon) = self.icon {
            // vector glyph centred on the disc at the shared DISC_ICON_RATIO box, so every round
            // control carries its icon at one ratio.
            let d = (r.w * DISC_ICON_RATIO).round();
            crate::ui::icons::draw(p, icon, Rect::new(r.cx() - d * 0.5, r.y + (r.h - d) * 0.5, d, d), ink);
        } else {
            // text glyph centred on the disc by its cap band (layout ≠ paint), not a hand-tuned y
            crate::ui::label::Label::new(self.glyph, crate::ui::theme::size::HEADLINE, ink)
                .h(crate::ui::label::HAlign::Center)
                .draw(p, r);
        }
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
    /// the lit dot (0-based); clamped into range so a stale index degrades to the last dot.
    pub fn active(mut self, i: usize) -> Self {
        self.active = if self.count == 0 { 0 } else { i.min(self.count - 1) };
        self
    }
    pub fn at(mut self, x: f32, y: f32) -> Self {
        self.x = x;
        self.y = y;
        self
    }
}
impl View for PageDots {
    fn draw(&self, _e: &Env, p: Painter) {
        const GAP: f32 = 12.0; // equal edge-gap between every element (dot↔dot and dot↔pill)
        const DOT: f32 = 10.0; // inactive diameter (also the pill height)
        const PILL: f32 = 24.0; // active pill width
        // Advance by each element's own width + a fixed gap, so the gaps are equal even around the
        // wider active pill (equal centre-pitch would squeeze the pill's neighbours). The pill just
        // takes more width and nudges the trailing dots along.
        let mut x = self.x;
        for d in 0..self.count {
            let active = d == self.active;
            let w = if active { PILL } else { DOT };
            // tokens, not a raw literal: full-strength white for the current page, dimmed for the rest
            let col = crate::ui::theme::with_a(crate::ui::theme::TEXT_PRIMARY, if active { 1.0 } else { 0.35 });
            p.rect(Rect::new(x, self.y, w, DOT), DOT * 0.5, col, col, 0.0);
            x += w + GAP;
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
        // dot size scales WITH the ring radius (0.28·r ≈ the HUD spinner's original 3.4 at r=12),
        // so a big spinner reads as a bigger spinner, not the same tiny dots on a wider circle.
        Self { cx, cy, r, phase: 0, col: [1.0, 1.0, 1.0, 1.0], dots: 10, dot_r: (r * 0.28).max(3.0) }
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
        let s = (r.w * DISC_ICON_RATIO).round();
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
        // Legibility over art is the CONTAINER's job (the tab-bar track in `draw_tab_row`), not
        // the segment's — the detail season tabs use this element bare on a controlled ground.
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

// ---- The shared top tab row: profile chip leads at the margin, the pills (Home | <library
// sections>) sit CENTERED — the tvOS tab-bar idiom. Drawn by BOTH the Home screen and the
// Library screen so they read as one global tab bar; the pill rects live here so both
// screens' pointer paths share [`tab_pill_at`]. ----
/// Home + up to 4 sections. Focus walking and the rects clamp to this — a pill that can't be
/// drawn must never be focusable.
pub(crate) const MAX_TABS: usize = 5;
/// The top chrome band's y — the chip and the pills sit on it (Home and Library alike).
pub(crate) const TOP_BAR_Y: f32 = 44.0;
/// SAME element, SAME geometry as the detail season tabs (user directive): one control height
/// (the 60px circle-button CD family) and the season tabs' ±18 label padding — the two rows
/// must be indistinguishable as a control.
const TAB_PILL_H: f32 = 60.0;
const TAB_PILL_PAD: f32 = 18.0;
static mut PILL_RECTS: [Rect; MAX_TABS] = [Rect::new(0.0, 0.0, 0.0, 0.0); MAX_TABS];
static mut N_PILLS: usize = 0;
// label + width cache keyed on browse::sections_gen(): rebuilding the CStrings and
// re-measuring every frame — on Home's hot path too — was a review-confirmed waste
static mut TAB_CACHE: Option<(u32, Vec<std::ffi::CString>, Vec<f32>)> = None;

/// The tab pill under the pointer (0 = Home, 1.. = sections), or None.
pub(crate) fn tab_pill_at(mx: f32, my: f32) -> Option<usize> {
    let n = unsafe { std::ptr::addr_of!(N_PILLS).read() };
    let rects = unsafe { std::ptr::addr_of!(PILL_RECTS).read() };
    rects[..n].iter().position(|r| r.contains(mx, my))
}

/// Draw the centered pill row. `selected` = the tab whose screen is showing (0 = Home);
/// `focused` = the pill holding remote focus, or -1. Records the rects for [`tab_pill_at`].
pub(crate) fn draw_tab_row(p: Painter, selected: std::os::raw::c_int, focused: std::os::raw::c_int) {
    use std::ffi::CString;
    use std::ptr::addr_of_mut;
    let gen = crate::browse::sections_gen();
    let cache = unsafe { &mut *addr_of_mut!(TAB_CACHE) };
    if cache.as_ref().map(|c| c.0 != gen).unwrap_or(true) {
        let nsec = crate::browse::section_count().min(MAX_TABS - 1);
        let mut labels: Vec<CString> = Vec::with_capacity(1 + nsec);
        labels.push(CString::new("Home").unwrap_or_default());
        for i in 0..nsec {
            labels.push(CString::new(crate::browse::section_title(i)).unwrap_or_default());
        }
        // measure bold (the widest state) so pill widths don't change with focus; pill =
        // label + the season tabs' ±18 padding
        let widths: Vec<f32> = labels
            .iter()
            .map(|l| crate::text::text_width(l.as_ptr(), theme::size::BODY, 1) + 2.0 * TAB_PILL_PAD)
            .collect();
        *cache = Some((gen, labels, widths));
    }
    let (_, labels, widths) = cache.as_ref().unwrap();
    let n = labels.len();
    // inter-pill air ≈ the season tabs' rhythm (their TAB_ADVANCE 52 minus the 2×18 pad the
    // pills here already carry — pill edge to pill edge reads the same)
    let gap = 16.0f32;
    let total_w: f32 = widths.iter().sum::<f32>() + gap * (n as f32 - 1.0);
    let x0 = (crate::ui::consts::SCR_W - total_w) * 0.5;
    // ONE translucent dark capsule contains the whole row — the tvOS tab-bar track. It (not the
    // segments) owns legibility over bright hero art: inside it the segments keep their clean
    // season-tab looks (plain = bare dim text). Sheened for the 1px glass rim.
    // UNIFORM 8px inset on every side: the pill (r=30) and the track (r=38) stay CONCENTRIC
    // (outer radius = inner radius + gap), so an end pill's corner gap reads even all the way
    // around — 16px ends against 8px verticals looked lopsided on the selected Home pill.
    const TRACK_PAD: f32 = 8.0;
    let track = Rect::new(
        x0 - TRACK_PAD,
        TOP_BAR_Y - TRACK_PAD,
        total_w + 2.0 * TRACK_PAD,
        TAB_PILL_H + 2.0 * TRACK_PAD,
    );
    // dark-material weight: light enough to keep a hint of the art, dark enough that the
    // TEXT_TERTIARY plain segments hold contrast even over pure-white art (Toy Story 2)
    p.rect_sheened(track, track.h * 0.5, theme::scrim_black(0.72), theme::scrim_black(0.82));
    let mut x = x0;
    let env = Env::inert();
    unsafe { N_PILLS = n };
    for i in 0..n {
        let r = Rect::new(x, TOP_BAR_Y, widths[i], TAB_PILL_H);
        unsafe { (*std::ptr::addr_of_mut!(PILL_RECTS))[i] = r };
        TabPill::new(labels[i].as_ptr(), theme::size::BODY, r)
            .segment(i as c_int == selected)
            .focused(i as c_int == focused)
            .draw(&env, p);
        x += widths[i] + gap;
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

// ---- Badge: the small rounded metadata chip (CC / SDH / AD / FORCED / codec tags). ONE leaf for
// the track-menu rows, the Info card meta line, and the detail About column, so the chip look
// can't drift. Cap-band-centred bold CAPTION label; width hugs the label with a floor so short
// tags (CC) still read as a chip. Returns the drawn width so callers can flow chips inline. ----
pub(crate) enum BadgeStyle {
    /// 2px border + knockout interior: border+label in `col`, interior filled `bg` (the surface
    /// behind the chip — keeps the outline clean over a light focus pill or a dark panel).
    Outlined { col: [f32; 4], bg: [f32; 4] },
    /// solid translucent fill ([`theme::BADGE_FILL`]), label in [`theme::TEXT_HEADING`] — the
    /// About column's accessibility chips.
    Filled,
}
/// pixel width [`badge`] will occupy for `text` — the layout companion (e.g. reserving the
/// inline-chip run so a row label elides before it).
pub(crate) fn badge_w(text: &str) -> f32 {
    const PAD: f32 = 12.0;
    const MIN_W: f32 = 56.0;
    std::ffi::CString::new(text)
        .ok()
        .map(|c| (crate::text::text_width(c.as_ptr(), theme::size::CAPTION, 1) + 2.0 * PAD).max(MIN_W))
        .unwrap_or(0.0)
}
/// Draw one chip with its LEFT edge at `x`, vertically centred on `cy`; returns its width.
pub(crate) fn badge(p: Painter, x: f32, cy: f32, text: &str, style: BadgeStyle) -> f32 {
    const H: f32 = 34.0;
    let lc = match std::ffi::CString::new(text) {
        Ok(c) => c,
        Err(_) => return 0.0,
    };
    let sz = theme::size::CAPTION;
    let w = badge_w(text);
    let r = Rect::new(x, cy - H * 0.5, w, H);
    let ink = match style {
        BadgeStyle::Outlined { col, bg } => {
            let bw = 2.0f32;
            p.rrect(r, 6.0, 6.0, col); // border
            p.rrect(Rect::new(r.x + bw, r.y + bw, r.w - 2.0 * bw, r.h - 2.0 * bw), 5.0, 5.0, bg);
            col
        }
        BadgeStyle::Filled => {
            p.rrect(r, 7.0, 7.0, theme::BADGE_FILL);
            theme::TEXT_HEADING
        }
    };
    let ty = crate::text::text_vcenter_y(sz, 1, cy);
    p.text(lc.as_ptr(), x + w * 0.5, ty, sz, ink, 1, 1);
    w
}
