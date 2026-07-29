//! Reusable retui leaves + shared helpers. These are the "reusable UI elements":
//! Button, CircleButton, TabPill, TransportButton, PageDots, Badge, plus the shared art-card
//! core (`card`/`draw_card`) and the poster-resolve helper. (Multi-line text wrapping
//! now lives in the `TextView` primitive in `text_view.rs`.)
use crate::pms::PmsMovie;
use crate::ui::theme;
use crate::ui::label::{HAlign, Label};
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

/// The profile chip's diameter: ONE control height with the tab pills and the circle-button
/// family, so the focused chip's capsule — the avatar plus [`TAB_TRACK_PAD`] all round — is
/// exactly the tab-bar track's band, and the two sit concentric on the top chrome line.
pub(crate) const CHIP_D: f32 = TAB_PILL_H;
/// Avatar → name air inside the expanded chip, and the capsule's tail past the name. The tail is
/// bigger so the name isn't crowded against the round end (the tab track reads the same: its own
/// 8px inset plus the end pill's 18px label padding).
const CHIP_NAME_GAP: f32 = 14.0;
const CHIP_NAME_TAIL: f32 = 24.0;
/// Name budget — a long profile name elides rather than growing the capsule into the CENTERED tab
/// track sitting a few hundred px to its right.
const CHIP_NAME_MAX: f32 = 320.0;

/// The top-left profile chip's VISUAL (avatar texture, or an initial / person-glyph fallback,
/// with the shared tile shadow + sheen). Shared by Home and the Library screen — each screen owns
/// its rect + focus rule and calls this. The session lookup (mutex + UserRef clone) is
/// snapshotted per profile GENERATION, not per frame.
///
/// `expand` (0..1) is the focus amount, deliberately a scalar and not a bool: at 1 the chip grows
/// the **tab bar's own track capsule** around itself and unfurls the profile name to its right. A
/// lifted shadow alone was far too quiet to read as focus against bright hero art — and the name
/// is what the stop is actually *for*. The caller owns the spring (Home steps it; the Library
/// screen has no chip focus stop and passes 0).
pub(crate) fn profile_chip(p: Painter, r: Rect, expand: f32) {
    use std::ffi::CString;
    use std::ptr::addr_of_mut;
    static mut CHIP: Option<(u32, String, CString, CString, f32)> = None; // gen, thumb, initial, name, name w
    let d = r.w;
    let gen = crate::plex::session::current_gen();
    let chip = unsafe { &mut *addr_of_mut!(CHIP) };
    if chip.as_ref().map(|c| c.0 != gen).unwrap_or(true) {
        let cur = crate::plex::session::current();
        let thumb = cur.as_ref().map(|u| u.thumb.clone()).unwrap_or_default();
        let title = cur.as_ref().map(|u| u.title.clone()).unwrap_or_default();
        let initial =
            title.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
        // signed out: the menu behind the chip offers Sign in, so the expanded chip says so too
        let label = if title.is_empty() { "Sign in".to_string() } else { title };
        let name = CString::new(crate::text::elide(&label, CHIP_NAME_MAX, theme::size::BODY, 1, false))
            .unwrap_or_default();
        let nw = crate::text::text_width(name.as_ptr(), theme::size::BODY, 1);
        *chip = Some((gen, thumb, CString::new(initial).unwrap_or_default(), name, nw));
    }
    let (_, thumb_s, initial_c, name_c, name_w) = chip.as_ref().unwrap();
    // ---- the focused capsule, UNDER the avatar: the tab track's material, inset and radius,
    // widened from a bare disc surround to hold the name.
    let e = expand.clamp(0.0, 1.0);
    if e > 0.004 {
        let closed = d + 2.0 * TAB_TRACK_PAD;
        let open = closed + CHIP_NAME_GAP + name_w + CHIP_NAME_TAIL;
        let cap = Rect::new(
            r.x - TAB_TRACK_PAD,
            r.y - TAB_TRACK_PAD,
            closed + (open - closed) * e,
            d + 2.0 * TAB_TRACK_PAD,
        );
        p.rect_sheened(cap, cap.h * 0.5, theme::scrim_black(0.72 * e), theme::scrim_black(0.82 * e));
        // the name rides in on the TAIL of the widening, so the glyphs land in a capsule that has
        // already made room for them instead of smearing across the grow
        let na = ((e - 0.55) / 0.45).clamp(0.0, 1.0);
        if na > 0.004 {
            let ty = crate::text::text_vcenter_y(theme::size::BODY, 1, r.cy());
            p.alpha(na).text(
                name_c.as_ptr(),
                r.x + d + CHIP_NAME_GAP,
                ty,
                theme::size::BODY,
                theme::TEXT_PRIMARY,
                0,
                1,
            );
        }
    }
    // resting shadow + perimeter stroke always; lift the shadow with the focus (same as shelf tiles)
    p.focus_shadow(r, d * 0.5, e);
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
    const GAP: f32 = 12.0; // equal edge-gap between every element (dot↔dot and dot↔pill)
    const DOT: f32 = 10.0; // inactive diameter (also the pill height)
    const PILL: f32 = 24.0; // active pill width

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
    /// Place the row so it is CENTRED on `cx` — the billboard pager idiom, where the dots belong to
    /// the screen rather than to the control they sit under. Resolves to a left origin through
    /// [`width`](Self::width), so `draw` stays one left-to-right walk.
    pub fn centered_at(self, cx: f32, y: f32) -> Self {
        let w = self.width();
        self.at(cx - w * 0.5, y)
    }
    /// The row's drawn width — the SAME advance `draw` walks (each element's own width plus one
    /// gap between neighbours), so a centred row can't drift from the pixels.
    pub fn width(&self) -> f32 {
        if self.count == 0 {
            return 0.0;
        }
        (self.count - 1) as f32 * (Self::DOT + Self::GAP) + Self::PILL
    }
}
impl View for PageDots {
    fn draw(&self, _e: &Env, p: Painter) {
        // Advance by each element's own width + a fixed gap, so the gaps are equal even around the
        // wider active pill (equal centre-pitch would squeeze the pill's neighbours). The pill just
        // takes more width and nudges the trailing dots along.
        let mut x = self.x;
        for d in 0..self.count {
            let active = d == self.active;
            let w = if active { Self::PILL } else { Self::DOT };
            // tokens, not a raw literal: full-strength white for the current page, dimmed for the rest
            let col = crate::ui::theme::with_a(crate::ui::theme::TEXT_PRIMARY, if active { 1.0 } else { 0.35 });
            p.rect(Rect::new(x, self.y, w, Self::DOT), Self::DOT * 0.5, col, col, 0.0);
            x += w + Self::GAP;
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

// ---- StatusOverlay: a centred "something is happening / something failed" read-out — a Spinner
// (or nothing, for a terminal state) above one line of copy. The player HUD renders it from
// `player::state()` while the pipeline is Connecting/Buffering/Seeking, and for the Error state,
// which is what a black screen used to be. `kind` picks the treatment, not the words: the caller
// supplies the caption so the state machine stays the single source of that string. ----
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// in flight — spinner + secondary copy
    Working,
    /// terminal failure — no spinner, danger-tinted copy
    Failed,
}
pub struct StatusOverlay {
    pub frame: Rect,
    /// `&'static CStr` from `PlaybackState::caption()` — no lifetime hazard, no per-frame alloc.
    pub caption: &'static core::ffi::CStr,
    pub kind: StatusKind,
    pub phase: u32,
}
impl StatusOverlay {
    pub fn new(frame: Rect, caption: &'static core::ffi::CStr, kind: StatusKind) -> Self {
        Self { frame, caption, kind, phase: 0 }
    }
    /// ms clock driving the spinner's rotation (ignored by `Failed`)
    pub fn phase(mut self, ms: u32) -> Self {
        self.phase = ms;
        self
    }
}
impl View for StatusOverlay {
    fn draw(&self, e: &Env, p: Painter) {
        // spinner above, caption below, the pair centred on the frame
        const R: f32 = 22.0;
        let cy = self.frame.cy();
        let (tint, working) = match self.kind {
            StatusKind::Working => (theme::TEXT_SECONDARY, true),
            StatusKind::Failed => (theme::DANGER, false),
        };
        // Both branches centre the caption the same way — by Label's cap band (VAlign::Middle,
        // the default). Working straddles the frame centre with the spinner above it; Failed owns
        // the centre alone. Using the cap band for one and a line-box metric for the other put the
        // two states on different baselines in the same frame.
        let cap_h = crate::text::text_height(theme::size::BODY, 0);
        let cap_y = if working {
            Spinner::new(self.frame.cx(), cy - R - theme::space::XS, R)
                .phase(self.phase)
                .tint(tint)
                .draw(e, p);
            cy + theme::space::XS
        } else {
            cy - cap_h * 0.5
        };
        Label::new(self.caption.as_ptr(), theme::size::BODY, tint)
            .h(HAlign::Center)
            .draw(p, Rect::new(self.frame.x, cap_y, self.frame.w, cap_h));
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
/// UNIFORM inset from the tab-bar track to the pills inside it, on every side: the pill (r=30) and
/// the track (r=38) stay CONCENTRIC (outer radius = inner radius + gap), so an end pill's corner
/// gap reads even all the way around — 16px ends against 8px verticals looked lopsided on the
/// selected Home pill. The focused [`profile_chip`] wraps itself in the same inset, which is what
/// makes its capsule and this track one band.
pub(crate) const TAB_TRACK_PAD: f32 = 8.0;
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
    // season-tab looks (plain = bare dim text). Sheened for the 1px glass rim. The uniform inset
    // (and so the concentric radii) is [`TAB_TRACK_PAD`], shared with the focused profile chip.
    let track = Rect::new(
        x0 - TAB_TRACK_PAD,
        TOP_BAR_Y - TAB_TRACK_PAD,
        total_w + 2.0 * TAB_TRACK_PAD,
        TAB_PILL_H + 2.0 * TAB_TRACK_PAD,
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
    /// 0..1 left-to-right FILL sweep across the pill; None = an ordinary button.
    pub progress: Option<f32>,
}
impl Button {
    pub fn new(label: *const c_char, sz: c_int, frame: Rect) -> Self {
        Self { frame, label, sz, icon: None, focused: false, style: ControlStyle::Accent, progress: None }
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
    /// Turn the pill into its own countdown: `frac` of its width is filled with
    /// [`theme::CONTROL_SPENT_FILL`], the rest with the button's normal face, so time reads as a
    /// sweep across the control itself instead of a separate rail beside it.
    ///
    /// **Progress and focus are separate channels, deliberately.** The first version drew the
    /// filled part as the FOCUSED face and the rest as the idle one, which collapsed the two: a
    /// focused counting button was pixel-identical to an unfocused idle one at t=0, and the label's
    /// ink flipped at the sweep line — bisecting a word with a hard edge, which from a couch reads
    /// as a torn glyph atlas rather than a timer. Now the face is drawn once, at its true focus
    /// state, and only the FILL BEHIND the label changes — the ink never inverts.
    pub fn progress(mut self, frac: f32) -> Self {
        self.progress = Some(frac);
        self
    }

    /// The pill's filled background, including the countdown sweep when one is set.
    fn plate(&self, p: Painter, bg: [f32; 4]) {
        let r = self.frame;
        let rad = r.h * 0.5;
        p.rrect(r, rad, rad, bg);
        let Some(frac) = self.progress else { return };
        let w = r.w * frac.clamp(0.0, 1.0);
        if w <= 0.0 {
            return;
        }
        // Scissor so the sweep inherits the capsule's rounded ends instead of a square edge; it is
        // GLOBAL GL state, so it is set and cleared inside this one draw and never left armed.
        p.clip(Rect::new(r.x, r.y, w, r.h));
        p.rrect(r, rad, rad, theme::CONTROL_SPENT_FILL);
        p.clip_clear();
    }
}
impl View for Button {
    fn draw(&self, _e: &Env, p: Painter) {
        let r = self.frame;
        let (bg, ink) = self.style.colors(self.focused);
        // Every control in this family carries the card system's RESTING shadow. Without it an
        // ACCENT capsule over a white frame measures ~1.2:1 against its surround — the shape
        // vanishes and only the dark label survives, floating. The discs and the shelves already
        // solved this; the pills were the one control that hadn't.
        p.shadow(r, r.h * 0.5, theme::CARD_SHADOW_REST_BLUR, theme::CARD_SHADOW_REST_DY,
                 theme::with_a(theme::CARD_SHADOW, theme::CARD_SHADOW_REST_A));
        self.plate(p, bg);
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

// ---- Rating badge: one review score behind its provider's brand mark (Rotten Tomatoes' tomato /
// popcorn, IMDb's star, TMDB's score ring). Deliberately NOT a [`badge`] chip: a chip's border
// boxes in art that is already a distinct silhouette, and four boxed chips in a row shout over the
// hero. The mark is an icon MASK tinted the brand colour, the score is ordinary primary ink, and
// the pair reads as part of the metadata line it flows after. Returns the drawn width so a row can
// flow badges inline, with [`rating_badge_w`] as the measure-first companion (same contract as
// `badge`/`badge_w`). ----
/// Mark box (px). A little over the meta line's cap height so a 24-unit silhouette still resolves
/// at couch distance — the marks carry the state (fresh vs rotten), so they have to be legible.
const RATING_MARK: f32 = 34.0;
/// Mark → score gap. They are one unit, so the tightest rung.
const RATING_GAP: f32 = theme::space::XS;

/// pixel width [`rating_badge`] will occupy for `value` — measure before drawing so a row can stop
/// at a margin instead of running a badge off the panel.
pub(crate) fn rating_badge_w(value: &str) -> f32 {
    std::ffi::CString::new(value)
        .ok()
        .map(|c| RATING_MARK + RATING_GAP + crate::text::text_width(c.as_ptr(), theme::size::LABEL, 1))
        .unwrap_or(0.0)
}

/// Draw one rating badge with its LEFT edge at `x`, centred on `cy`; returns its width.
pub(crate) fn rating_badge(
    p: Painter,
    x: f32,
    cy: f32,
    mark: crate::ui::icons::Icon,
    tint: [f32; 4],
    value: &str,
) -> f32 {
    let vc = match std::ffi::CString::new(value) {
        Ok(c) => c,
        Err(_) => return 0.0,
    };
    crate::ui::icons::draw(p, mark, Rect::new(x, cy - RATING_MARK * 0.5, RATING_MARK, RATING_MARK), tint);
    let ty = crate::text::text_vcenter_y(theme::size::LABEL, 1, cy);
    p.text(vc.as_ptr(), x + RATING_MARK + RATING_GAP, ty, theme::size::LABEL, theme::TEXT_PRIMARY, 0, 1);
    // the MEASURED width, not what `text` reports it rasterized — `badge` calls `badge_w` for the
    // same reason: a draw and its measurer that can disagree will eventually be caught disagreeing
    rating_badge_w(value)
}
