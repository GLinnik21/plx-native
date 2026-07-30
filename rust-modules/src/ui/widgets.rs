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

/// Source art for a [`card`]: a catalog poster (resolved 250×375, dark gradient skeleton), any
/// keyed thumbnail at an explicit resolution (flat placeholder skeleton), or a person's headshot
/// (the same thumbnail, but its EMPTY case draws a person glyph rather than a blank tile).
pub(crate) enum Art<'a> {
    Poster(Option<&'a PmsMovie>),
    Thumb { key: &'a str, res: (c_int, c_int) },
    /// A credit's headshot (the Cast & Crew shelf). Distinct from [`Art::Thumb`] because a
    /// missing headshot is ROUTINE here — the server has one for most actors and for few crew —
    /// and an empty circle beside named circles reads as a broken image rather than as a person
    /// the metadata agent has no photo of.
    Person { key: &'a str, res: (c_int, c_int) },
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
        Art::Person { key, res } => {
            let t = resolve_tex(key, res.0, res.1, 0);
            if t != 0 {
                p.tex_carded(t, r, rad, theme::TINT_WHITE, f);
            } else {
                p.rrect_sheened(r, rad, theme::CARD_PLACEHOLDER);
                // Only for a person the server has NO headshot of — an unresolved texture with a
                // key behind it is merely still loading, and glyphing that would flash a "no
                // photo" mark on every tile of every page for the length of its fetch.
                if key.is_empty() {
                    // quantize the glyph box to 4px so the focus-pop animation reuses a handful of
                    // cached icon masks instead of rasterizing + uploading one per rounded pixel
                    // (same discipline as the unwatched angle above)
                    let d = ((r.w * PERSON_GLYPH_RATIO) / 4.0).round() * 4.0;
                    crate::ui::icons::draw(
                        p,
                        crate::ui::icons::Icon::User,
                        Rect::new(r.cx() - d * 0.5, r.cy() - d * 0.5, d, d),
                        theme::TEXT_TERTIARY,
                    );
                }
            }
        }
    }
}

/// Person-glyph box as a fraction of a headshot tile — the [`Art::Person`] fallback's one ratio.
/// Deliberately TIGHTER than [`DISC_ICON_RATIO`] (0.54, the ratio every disc *control* glyph uses,
/// and what the profile chip's own fallback works out to): a headshot tile is 190px, and 0.54 of it
/// is past `icons::tex_for`'s 96px rasterization clamp — the mask would be upscaled and soft. At
/// 0.44 the box lands at 84-88px across the whole focus pop, inside the clamp and crisp.
const PERSON_GLYPH_RATIO: f32 = 0.44;

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

/// The **watched** mark: an amber disc with the check knocked out of it in dark ink, drawn to fill
/// `r`. The **done** state of the tile progress vocabulary (see [`card`]) — a filled disc rather than
/// a dark one with an amber tick, because `Details Screen.dc.html` sets it INLINE at the head of the
/// episode still's state line, at ~20px, where a dark disc reads as a hole in the artwork and the
/// amber tick inside it is two shapes' worth of detail the size cannot carry. Filled, it is one
/// confident amber dot that resolves at couch distance.
///
/// Mutual exclusion is the CALLER's rule — PMS keeps a resume point on a re-started episode, so the
/// wire says both watched AND in-progress; see `ui::detail::ep_state`, which resolves the three
/// states into one mark.
pub(crate) fn watched_disc(p: Painter, r: Rect) {
    p.rect(r, r.w * 0.5, theme::RESUME_FILL, theme::RESUME_FILL, 0.0);
    // glyph on the shared round-control ratio, so the check sits in the disc like every other one
    let i = (r.w * DISC_ICON_RATIO).round();
    crate::ui::icons::draw(
        p,
        crate::ui::icons::Icon::Check,
        Rect::new(r.cx() - i * 0.5, r.cy() - i * 0.5, i, i),
        theme::INK_ON_RESUME,
    );
}

/// How many flat bands [`art_scrim`] uses for its corner region — see there for why they exist. 3 is
/// enough that the step between them is ~0.03 alpha, well under a visible edge.
const SCRIM_CORNER_BANDS: usize = 3;

/// **THE progress bar** — the app's one "how far into this am I" mark, on every tile shape: the bottom
/// band of the artwork itself, full-bleed edge to edge, square-ended fill, clipped to the card's own
/// rounded silhouette so the amber visibly wraps the bottom corner arcs (the mock draws two square
/// strips under a `overflow:hidden`, which is what that clip reproduces).
///
/// One function for the Continue Watching poster and the episode still, because it is meant to be the
/// SAME bar: the still used to draw an inset rounded capsule 16px up while a CW card drew this, so
/// "how far in am I" was two different objects on two screens of one app.
///
/// Two details are load-bearing, both learned on the panel:
///
/// * **The band is snapped to whole composited pixels.** `gfx::clip_set` truncates its scissor to
///   integer rows while the fill is antialiased at fractional coordinates, so an unsnapped band leaves
///   a hairline of either unscrimmed artwork or double-darkened scrim (see [`art_scrim`], same cause).
/// * **Track and fill never share a pixel.** They used to be drawn as full-width track, then fill over
///   it — two translucent fills (α .22 and α .95), each with its own antialiased edge, compositing on
///   the same pixels where the card's SDF coverage is partial. Along the bottom-LEFT corner arc that
///   sum came out brighter than either one alone: a 1–2px light fleck at the corner, which pulsed as
///   the focus pop moved the geometry. Splitting the band at the played fraction removes the overlap
///   rather than trying to tune around it.
pub(crate) fn progress_bar(p: Painter, card: Rect, rad: f32, h: f32, frac: f32) {
    let snap = |y: f32| crate::gfx::snap(y + p.dy) - p.dy;
    let bottom = card.y + card.h;
    let top = snap(bottom - h.min(card.h));
    if bottom <= top {
        return;
    }
    let right = card.x + card.w;
    // the split is a whole pixel too, so the fill's square end is a clean edge rather than a column
    // of half-covered amber
    let split = (card.x + card.w * frac.clamp(0.0, 1.0)).round().clamp(card.x, right);
    if split > card.x {
        p.clip(Rect::new(card.x, top, split - card.x, bottom - top));
        p.rrect(card, rad, rad, theme::RESUME_FILL);
    }
    if split < right {
        p.clip(Rect::new(split, top, right - split, bottom - top));
        p.rrect(card, rad, rad, theme::RESUME_TRACK);
    }
    p.clip_clear();
}

// ---- Keyline chip: the FINE-PRINT outlined chip — a hairline box round a very short label, sized to
// hug it. Its one job is the content rating beside an episode's air date (`18+` / `TV-MA`).
//
// Deliberately NOT `badge` + `BadgeStyle::Outlined`, which is the same shape two rungs up: that chip
// is built on `BADGE_H` (34) with 12px padding, a 2px keyline and a BOLD `CAPTION` label, because it
// exists to sit in a row of metadata chips and hold its own. Down here it has to sit beside a 22px
// air date as the dimmest thing on the tile, and at badge's weight it outweighed the episode TITLE
// above it. `BADGE_H` stays one band for every `BadgeStyle`, as documented — this is a different leaf,
// not a resized one.
const KEYLINE_PAD_X: f32 = 7.0;
const KEYLINE_PAD_Y: f32 = 6.0;
const KEYLINE_RAD: f32 = 5.0;
const KEYLINE_W: f32 = 1.5;

/// The width [`keyline_chip`] will occupy for `text` — the measure-first companion.
pub(crate) fn keyline_chip_w(text: &str) -> f32 {
    std::ffi::CString::new(text)
        .ok()
        .map(|c| crate::text::text_width(c.as_ptr(), theme::size::CAPTION, 0) + 2.0 * KEYLINE_PAD_X)
        .unwrap_or(0.0)
}

/// Draw a fine-print keyline chip with its LEFT edge at `x`, centred on `cy`; returns its width.
/// `bg` must be the surface actually BEHIND the chip: the SDF has no stroke-only mode, so the outline
/// is a knockout (keyline colour, then the interior inset by it) rather than a hollow ring.
pub(crate) fn keyline_chip(p: Painter, x: f32, cy: f32, text: &str, col: [f32; 4], bg: [f32; 4]) -> f32 {
    let lc = match std::ffi::CString::new(text) {
        Ok(c) => c,
        Err(_) => return 0.0,
    };
    let w = keyline_chip_w(text);
    let (ct, cb) = crate::text::text_cap_band(theme::size::CAPTION, 0);
    let h = (cb - ct) + 2.0 * KEYLINE_PAD_Y; // hugs the label's cap band rather than a fixed band
    let r = Rect::new(x, cy - h * 0.5, w, h);
    p.rrect(r, KEYLINE_RAD, KEYLINE_RAD, col);
    let b = KEYLINE_W;
    p.rrect(
        Rect::new(r.x + b, r.y + b, r.w - 2.0 * b, r.h - 2.0 * b),
        (KEYLINE_RAD - b).max(0.0),
        (KEYLINE_RAD - b).max(0.0),
        bg,
    );
    p.text(lc.as_ptr(), r.x + KEYLINE_PAD_X, crate::text::text_vcenter_y(theme::size::CAPTION, 0, cy), theme::size::CAPTION, col, 0, 0);
    w
}

/// The **bottom scrim** on a piece of artwork: `h` px of near-black fading upward to nothing, clipped
/// to the card's own rounded silhouette. This is what lets text sit directly ON a still with no chip
/// or capsule behind it (`Details Screen.dc.html`'s episode tiles) — a plain label over an arbitrary
/// video frame is a coin flip for legibility, and a capsule per label was the alternative the design
/// deliberately drops.
///
/// Two parts, because `Painter::rect`'s gradient is the only one we have and it would round all four
/// corners of a band: the straight-sided majority is one gradient quad, and the last `rad` px — the
/// only rows where the card's corner arcs bite — are flat bands scissored to the card silhouette
/// (`card_row::resume_bar`'s corner-wrapping trick). Set/clear are paired inside the call, per
/// `ui/CLAUDE.md`'s clip contract.
pub(crate) fn art_scrim(p: Painter, card: Rect, rad: f32, h: f32, a: f32) {
    let h = h.min(card.h);
    if h <= 0.0 {
        return;
    }
    // Snap every internal boundary to a whole COMPOSITED pixel (fold the painter translate, snap,
    // unfold — the same contract text and icon masks use, see `gfx::snap`).
    //
    // This is load-bearing, not tidiness. The gradient quad below is a HARD fill edge — `gfx::draw_rect`
    // takes its no-AA fast path at radius 0, deliberately, so scrims stay exactly their bounds — while
    // `gfx::clip_set` TRUNCATES its scissor box to integer rows. At a fractional seam those two
    // disagree by up to a pixel, and the row between them is covered by neither the gradient nor the
    // first band: the artwork shows through it as a bright hairline across the whole tile. It only
    // appears once the strip's scroll spring settles somewhere fractional, which is nearly always.
    let snap = |y: f32| crate::gfx::snap(y + p.dy) - p.dy;
    let bottom = card.y + card.h;
    let top = snap(bottom - h);
    let seam = snap(bottom - rad).max(top);
    if seam > top {
        p.rect(
            Rect::new(card.x, top, card.w, seam - top),
            0.0,
            theme::scrim(0.0),
            theme::scrim(a * (seam - top) / h),
            0.0,
        );
    }
    let end = snap(bottom);
    if end <= seam {
        return;
    }
    // Internal boundaries ABUT EXACTLY — every edge above is snapped, so `clip_set`'s integer
    // truncation tiles the boxes with neither gap nor overlap. Both failure modes are visible as a
    // hairline across the whole tile and they look nothing alike: a gap leaves one row of unscrimmed
    // artwork (BRIGHT), while an overlap makes two translucent scrims composite on one row —
    // 1-(1-.7)(1-.7) ≈ .91 against ~.7 either side — which is a BLACK one. Do not add slop here.
    let bh = (end - seam) / SCRIM_CORNER_BANDS as f32;
    for i in 0..SCRIM_CORNER_BANDS {
        let y0 = if i == 0 { seam } else { snap(seam + i as f32 * bh) };
        let y1 = if i + 1 == SCRIM_CORNER_BANDS { end } else { snap(seam + (i + 1) as f32 * bh) };
        if y1 <= y0 {
            continue;
        }
        let t = ((y0 + y1) * 0.5 - top) / h;
        // …with ONE exception: the last band runs a pixel past the card's own edge. The rounded rect
        // it fills ends there regardless, so it costs nothing, and it guarantees the final row is
        // covered even though the card's true bottom is fractional.
        let tail = if i + 1 == SCRIM_CORNER_BANDS { 1.0 } else { 0.0 };
        p.clip(Rect::new(card.x, y0, card.w, y1 - y0 + tail));
        p.rrect(card, rad, rad, theme::scrim(a * t));
    }
    p.clip_clear();
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
    /// One full revolution, in ms. Public because a caller that accumulates its own phase clock
    /// can only wrap it losslessly on a whole period (see `home.rs`'s `status_ms`).
    pub const PERIOD_MS: u32 = 760;
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
        let t = (self.phase % Self::PERIOD_MS) as f32 / Self::PERIOD_MS as f32;
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusKind {
    /// in flight — spinner + secondary copy
    Working,
    /// terminal failure — no spinner, danger-tinted copy
    Failed,
    /// nothing to show, and that is the server's honest ANSWER rather than a fault — no spinner,
    /// de-emphasized copy. Distinct from `Failed` on purpose: an empty library is not an error and
    /// must not wear the danger tint (Home's empty state; the Library grid's "nothing matches" is
    /// the same read-out, still hand-rolled as a bare Label).
    Empty,
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
            StatusKind::Empty => (theme::TEXT_TERTIARY, false),
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
// faint fill + dim ink. `TabPill::width(chars, sz)` sizes it to fit — NOT `Button::pill_w`, which
// budgets a Button's icon box and air. ----
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

/// The quiet annotation a [`TabPill`] can carry after its label — the detail page's season tabs use
/// it for "you have watched everything behind this tab". It is deliberately subordinate to the
/// label: it stands one type rung down and a step of alpha under the label's own ink, so a tab
/// still reads as its name first and its state second.
///
/// It carried an episode COUNT too, and no longer does (owner call, 2026-07-29: *"I don't like
/// unwatched episodes count in season tab selector. I think it's ok to leave just marks that we
/// watched."*). A count is filing data the season's own episode row already answers by simply
/// existing, and it made every tab wider for a number nobody was reading. The tick is the one fact
/// a tab strip can state that its content cannot: which seasons are behind you.
#[derive(Clone, Copy, Default)]
pub enum TabNote {
    #[default]
    None,
    /// everything behind this tab is watched — a small tick after the label.
    Done,
}

/// Type rung the trailing note is measured against: one below the label, at the couch legibility
/// floor. The tick is a glyph rather than text, but it stands in the same band a label at this rung
/// would, so the note reads as a peer of the name it follows.
const NOTE_SZ: c_int = theme::size::CAPTION;
/// Label → note air inside the pill.
const NOTE_GAP: f32 = theme::space::SM;
/// The watched tick stands as tall as the note's own rung.
const NOTE_TICK_D: f32 = NOTE_SZ as f32;
/// The note's ink is the pill's own ink one alpha step down — de-emphasis without a second colour
/// role, so it stays legible in every one of [`TabStyle`]'s ink states (including the already-dim
/// unselected segment) while never competing with the label.
const NOTE_INK_A: f32 = 0.75;

// ---- TabPill: a rounded pill with a centered label, in one of two state models (TabStyle). ----
pub struct TabPill {
    pub frame: Rect,
    pub label: *const c_char,
    pub sz: c_int,
    pub focused: bool,
    style: TabStyle,
    note: TabNote,
    /// see [`TabPill::plated`]
    plated: bool,
}
impl TabPill {
    /// pill width for a `chars`-long label at `sz` (label advance + horizontal padding). Covers
    /// the LABEL only — a pill that also carries a [`TabNote`] must add [`note_w`](Self::note_w),
    /// or the note is drawn outside the fill.
    pub fn width(chars: usize, sz: c_int) -> f32 {
        chars as f32 * sz as f32 * 0.56 + 44.0
    }
    pub fn new(label: *const c_char, sz: c_int, frame: Rect) -> Self {
        Self { frame, label, sz, focused: false, style: TabStyle::Button, note: TabNote::None, plated: false }
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
    /// switch to the segmented-control look (detail season tabs); `selected` = the active segment.
    /// Give every segment its own faint plate, and the selected one a stronger plate
    /// (`Details Screen.dc.html`'s season tabs). **Opt-in**, because the TOP tab row draws its segments
    /// inside the tab-bar TRACK — that track already is the ground, and plating there would stack two.
    /// The detail page's season tabs have no track, so without this an unselected season was bare text
    /// with no indication it could be pressed.
    pub fn plated(mut self) -> Self {
        self.plated = true;
        self
    }
    pub fn segment(mut self, selected: bool) -> Self {
        self.style = TabStyle::Segment { selected };
        self
    }
    /// attach a trailing [`TabNote`] (the watched tick).
    pub fn note(mut self, note: TabNote) -> Self {
        self.note = note;
        self
    }
    /// What the note adds to a pill's CONTENT width, gap included (0 for [`TabNote::None`]). The
    /// caller lays its tab strip out with this, so pill widths, the strip's x-advance and the note
    /// itself can never disagree about how wide a tab is.
    pub fn note_w(note: TabNote) -> f32 {
        match note {
            TabNote::None => 0.0,
            TabNote::Done => NOTE_GAP + NOTE_TICK_D,
        }
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
            TabStyle::Segment { selected: true } if self.plated => (Some(theme::TAB_PLATE_SELECTED), theme::TEXT_PRIMARY, true),
            TabStyle::Segment { .. } if self.plated => (Some(theme::TAB_PLATE_IDLE), theme::TEXT_TERTIARY, false),
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
        // The label centres in the pill MINUS the note group, so a tab carrying a note keeps
        // label+note centred as one unit rather than shoving the label off-centre. The note then
        // hugs the label's own PAINTED right edge (the width `Label::draw` hands back), which is
        // why this needs no knowledge of the caller's pill padding.
        //
        // Painted, deliberately, not the width the caller measured: a strip that sizes its tabs
        // with the BOLD label (as the season tabs and `draw_tab_row` both do, so an advance can't
        // move with focus) paints a NON-bold label a couple of px narrower. Anchoring off the
        // caller's number instead would pin the note while the label's right edge slid under it,
        // i.e. it would trade a constant label→note gap for a variable one. The gap is the
        // relationship the eye reads here; the few px of slack left inside the pill's own padding
        // on an unbolded tab is the same slack the label alone already had.
        let nw = Self::note_w(self.note);
        let lw = lab.draw(p, Rect::new(r.x, r.y, r.w - nw, r.h));
        let nx = r.x + (r.w - nw) * 0.5 + lw * 0.5 + NOTE_GAP;
        let note_ink = theme::with_a(ink, NOTE_INK_A);
        match self.note {
            TabNote::None => {}
            TabNote::Done => {
                let d = NOTE_TICK_D;
                crate::ui::icons::draw(
                    p,
                    crate::ui::icons::Icon::Check,
                    Rect::new(nx, r.y + (r.h - d) * 0.5, d, d),
                    note_ink,
                );
            }
        }
    }
}

// ---- The shared top tab row: profile chip leads at the margin, the pills (Home | <library
// sections>) sit CENTERED — the tvOS tab-bar idiom. Drawn by BOTH the Home screen and the
// Library screen so they read as one global tab bar; the pill rects live here so both
// screens' pointer paths share [`tab_pill_at`]. ----
/// Home + **every** discovered library section. Focus walking clamps to this — the invariant is
/// still "a pill that can't be drawn must never be focusable", but it now holds by construction
/// rather than by a cap: the strip scrolls horizontally inside its track (see
/// [`tab_scroll_target`]), so every pill can be brought into view and every pill is focusable.
/// This used to be a hard `MAX_TABS = 5`, which left a fifth `movie`/`show` section unreachable
/// from the UI even though `browse` discovers it and the grid browses it fine.
pub(crate) fn tab_count() -> usize {
    1 + crate::browse::section_count()
}
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
/// Inter-pill air ≈ the season tabs' rhythm (their `TAB_ADVANCE` 52 minus the 2×18 pad the pills
/// here already carry — pill edge to pill edge reads the same). Doubles as the scroll-into-view
/// context margin, exactly as the season tabs use their advance.
const TAB_GAP: f32 = 16.0;
/// How wide the pill strip may grow before it starts scrolling. The row stays CENTERED, but it
/// must never reach the profile chip (a focus stop of its own at `MARGIN_X`), so the viewport is
/// the screen less a symmetric chip-clearing margin, less the track's own inset on both ends.
const TAB_SIDE_CLEAR: f32 = crate::ui::consts::MARGIN_X + CHIP_D + theme::space::MD;
const TAB_VIEW_MAX: f32 = crate::ui::consts::SCR_W - 2.0 * (TAB_SIDE_CLEAR + TAB_TRACK_PAD);
/// The strip's scroll stiffness. The detail page's season-tab row springs at the same 240 — same
/// control, same overflow problem, so the two rows must move alike (the user directive that keeps
/// the tab pills and the season tabs in step covers their motion, not just their geometry).
const K_TAB_SCROLL: f32 = 240.0;
/// The VISIBLE part of each pill as drawn this frame (scroll folded in, then intersected with the
/// strip's viewport), one entry per pill — never a fixed array's worth, or the hit test would stop
/// where the old cap did. Storing the *clipped* rect is what keeps the hit test and the scissor
/// telling the same story: a pill scrolled out of the track has zero width here, so it is neither
/// drawn nor clickable, and a half-visible one is clickable exactly across the half you can see.
static mut PILL_RECTS: Vec<Rect> = Vec::new();
/// Horizontal scroll of the strip inside its track; 0 whenever the whole row fits.
static mut TAB_SCROLL: crate::ui::Spring = crate::ui::Spring::at(0.0);
// label + width cache keyed on browse::sections_gen(): rebuilding the CStrings and
// re-measuring every frame — on Home's hot path too — was a review-confirmed waste
static mut TAB_CACHE: Option<(u32, Vec<std::ffi::CString>, Vec<f32>)> = None;

/// The tab pill under the pointer (0 = Home, 1.. = sections), or None. Matches against the
/// CLIPPED rects, so only the pill area you can actually see is clickable.
///
/// One consequence worth knowing: these rects are the last DRAWN frame's, so a click that lands
/// while the strip is mid-reveal is graded against where the pills were when the user last saw
/// them — which is the right frame to grade against, but does mean a click aimed at a pill the
/// spring is still carrying can land on its neighbour. The strip only moves in response to the
/// user's own focus move, so the two gestures do not overlap in practice.
pub(crate) fn tab_pill_at(mx: f32, my: f32) -> Option<usize> {
    let rects = unsafe { &*std::ptr::addr_of!(PILL_RECTS) };
    rects.iter().position(|r| r.w > 0.5 && r.contains(mx, my))
}

/// Run `f` with the tab row's labels + pill widths, rebuilding them only when
/// `browse::sections_gen()` moves. Shared by the per-frame scroll step and the draw, so the two
/// can't measure the strip differently.
///
/// Deliberately a closure and not a `&'static` getter: the borrow points into `TAB_CACHE`, and a
/// nested rebuild would free the `CString`s a caller is still handing to `TabPill` as a raw
/// `*const c_char`. Scoping it here makes that impossible to write by accident — so do NOT call
/// this again from inside `f`.
fn with_tab_metrics<R>(f: impl FnOnce(&[std::ffi::CString], &[f32]) -> R) -> R {
    use std::ffi::CString;
    use std::ptr::addr_of_mut;
    let gen = crate::browse::sections_gen();
    let cache = unsafe { &mut *addr_of_mut!(TAB_CACHE) };
    if cache.as_ref().map(|c| c.0 != gen).unwrap_or(true) {
        let nsec = crate::browse::section_count();
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
    f(labels, widths)
}

/// Content width of the whole pill strip: the pills plus the air between them.
fn tab_content_w(widths: &[f32]) -> f32 {
    widths.iter().sum::<f32>() + TAB_GAP * (widths.len() as f32 - 1.0).max(0.0)
}
/// The VISIBLE pill area: the strip's own width until it outgrows [`TAB_VIEW_MAX`], then that.
fn tab_view_w(widths: &[f32]) -> f32 {
    tab_content_w(widths).min(TAB_VIEW_MAX).max(0.0)
}
/// Content-space x of pill `i`'s left edge (0 = the strip's start).
fn tab_pill_x(widths: &[f32], i: usize) -> f32 {
    let i = i.min(widths.len());
    widths[..i].iter().sum::<f32>() + TAB_GAP * i as f32
}
/// Scroll offset that brings pill `idx` into the strip's viewport: the minimal scroll-into-view
/// rule the season tabs and every shelf share ([`card_row::reveal`]) — move only when the pill
/// (± one gap of context) would clip, and never past the content ends. Pure, so the "every pill
/// is reachable" invariant is host-testable without a font.
fn tab_scroll_target(widths: &[f32], idx: usize, cur: f32) -> f32 {
    let view_w = tab_view_w(widths);
    let max = (tab_content_w(widths) - view_w).max(0.0);
    if idx >= widths.len() {
        return cur.clamp(0.0, max);
    }
    let x = tab_pill_x(widths, idx);
    let lo = x + widths[idx] + TAB_GAP - view_w; // right edge (+ context) on screen
    let hi = x - TAB_GAP; // left edge (− context) on screen
    crate::ui::card_row::reveal(cur, lo, hi, max)
}

/// Step the strip's horizontal scroll — called once per frame from BOTH screens' update (the draw
/// runs at dt=0, like the profile chip's unfurl). `focused` = the pill holding remote focus or -1,
/// `selected` = the tab whose screen is showing.
///
/// Off the row it tracks the SELECTED pill, and on Home that means the strip returns to the start
/// when focus leaves the band. That is deliberate, and it is where this differs from the season
/// tabs (which HOLD): the way back INTO the band is the profile chip and then the Home pill — its
/// left end — so a strip parked far to the right would be showing pills that the next keypress
/// cannot reach without scrolling back anyway, under a row with no selected tab visible. Reveal
/// is minimal-scroll, so this is a no-op whenever the selected pill is already on screen, which is
/// the whole of the Library screen's life after [`tab_row_reveal`] placed it.
pub(crate) fn tab_row_update(selected: c_int, focused: c_int, dt: f32) {
    use std::ptr::{addr_of, addr_of_mut};
    let idx = if focused >= 0 { focused } else { selected.max(0) } as usize;
    let cur = unsafe { addr_of!(TAB_SCROLL).read() };
    let t = with_tab_metrics(|_, w| tab_scroll_target(w, idx, cur.pos));
    unsafe { (*addr_of_mut!(TAB_SCROLL)).step(t, K_TAB_SCROLL, dt) };
    let s = unsafe { addr_of!(TAB_SCROLL).read() };
    crate::ui::anim::probe("tabrow.scroll", s.pos, s.vel, t, dt);
}

/// Put pill `idx` on screen at once (no glide). For screen ENTRY — the Library screen opened
/// straight into a far-right section (the `/tmp/plxnative-library=N` boot, or a section restored
/// from the saved view) must show its own tab, and a long slide on arrival would be motion the
/// user never asked for. Mirrors `detail.rs`'s `tab_hscroll.jump` on a fresh detail page.
pub(crate) fn tab_row_reveal(idx: usize) {
    use std::ptr::{addr_of, addr_of_mut};
    let cur = unsafe { addr_of!(TAB_SCROLL).read() };
    let t = with_tab_metrics(|_, w| tab_scroll_target(w, idx, cur.pos));
    unsafe { (*addr_of_mut!(TAB_SCROLL)).jump(t) };
}

/// Draw the centered pill row. `selected` = the tab whose screen is showing (0 = Home);
/// `focused` = the pill holding remote focus, or -1. Records the rects for [`tab_pill_at`].
pub(crate) fn draw_tab_row(p: Painter, selected: std::os::raw::c_int, focused: std::os::raw::c_int) {
    use std::ptr::{addr_of, addr_of_mut};
    let rects = unsafe { &mut *addr_of_mut!(PILL_RECTS) };
    rects.clear(); // nothing drawn = nothing hittable, including on the early return below
    with_tab_metrics(|labels, widths| {
        let n = labels.len();
        if n == 0 {
            return;
        }
        let content_w = tab_content_w(widths);
        let view_w = tab_view_w(widths);
        // ONE translucent dark capsule contains the whole row — the tvOS tab-bar track. It (not the
        // segments) owns legibility over bright hero art: inside it the segments keep their clean
        // season-tab looks (plain = bare dim text). Sheened for the 1px glass rim. The uniform inset
        // (and so the concentric radii) is [`TAB_TRACK_PAD`], shared with the focused profile chip.
        // The track is the strip's width until the strip outgrows the screen, then it caps and the
        // pills scroll inside it — the track itself never moves.
        let x0 = (crate::ui::consts::SCR_W - view_w) * 0.5;
        let track = Rect::new(
            x0 - TAB_TRACK_PAD,
            TOP_BAR_Y - TAB_TRACK_PAD,
            view_w + 2.0 * TAB_TRACK_PAD,
            TAB_PILL_H + 2.0 * TAB_TRACK_PAD,
        );
        // dark-material weight: light enough to keep a hint of the art, dark enough that the
        // TEXT_TERTIARY plain segments hold contrast even over pure-white art (Toy Story 2)
        p.rect_sheened(track, track.h * 0.5, theme::scrim_black(0.72), theme::scrim_black(0.82));
        // A strip wider than its track is a bounded panel, not a scrolling document, so this is the
        // scissor case (see the ui/CLAUDE.md clipping rule): a pill leaving the row is cut at the
        // pill area's edge — which is also the "there is more over there" affordance. Paired below.
        // A non-zero scroll clips too even when the strip now fits: the section table can shrink
        // (sign-out, server switch) and the spring takes a few frames to unwind, and those frames
        // must not paint pills outside the track.
        let view = Rect::new(x0, TOP_BAR_Y, view_w, TAB_PILL_H);
        let sx = unsafe { addr_of!(TAB_SCROLL).read() }.pos;
        let scrolls = content_w > view_w + 0.5 || sx.abs() > 0.5;
        if scrolls {
            p.clip(view);
        }
        let env = Env::inert();
        rects.reserve(n);
        for i in 0..n {
            let r = Rect::new(x0 + tab_pill_x(widths, i) - sx, TOP_BAR_Y, widths[i], TAB_PILL_H);
            // ONE rule for "is this pill on screen": its rect clipped to the viewport. The clipped
            // rect is what gets recorded for the hit test AND what decides whether to draw at all
            // (a 12-library server would otherwise lay out three rows' worth of text the scissor
            // throws away), so the two can never disagree about a sliver at the edge.
            let vis = r.intersect(view);
            rects.push(vis);
            if vis.w > 0.5 {
                TabPill::new(labels[i].as_ptr(), theme::size::BODY, r)
                    .segment(i as c_int == selected)
                    .focused(i as c_int == focused)
                    .draw(&env, p);
            }
        }
        if scrolls {
            p.clip_clear();
        }
    })
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
/// [`Button`]'s icon box, as a multiple of its type size, and the icon→label gap. Named because
/// [`Button::pill_w`] measures the same run `Button::draw` lays out — a literal in each would let
/// the two drift.
const BTN_ICON_RATIO: f32 = 1.15;
const BTN_ICON_GAP: f32 = 12.0;
/// Total horizontal air a pill carries around its icon+label run.
const BTN_PILL_AIR: f32 = 68.0;

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

    /// The width this button wants for `label` at type size `sz`: its own content run (icon box +
    /// gap + label, per [`BTN_ICON_RATIO`]/[`BTN_ICON_GAP`]) plus one air budget. The LAYOUT
    /// companion to `draw`, which only ever centres that same run in the frame it is handed — so
    /// the two read the same constants and cannot drift.
    ///
    /// ONE formula for every pill in the product, because they all relabel from state and a fixed
    /// frame that fits the short word crams the long one against its own capsule ends: both hero
    /// rows ("Play"/"Continue", "Play"/"Resume") pass `icon: true`, and Home's status-screen Retry
    /// control passes `false`. That flag is the whole reason this takes one — an icon-less pill
    /// measured with an icon box gets a slug of air it never fills, which is why the earlier
    /// icon-only version had to send such callers off to `text::text_width` on their own. A second
    /// sizing path is exactly the drift this file exists to prevent.
    pub fn pill_w(label: *const c_char, sz: c_int, icon: bool) -> f32 {
        let (isz, gap) = if icon { (sz as f32 * BTN_ICON_RATIO, BTN_ICON_GAP) } else { (0.0, 0.0) };
        isz + gap + crate::text::text_width(label, sz, 1) + BTN_PILL_AIR
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
        let (isz, gap) =
            if self.icon.is_some() { (self.sz as f32 * BTN_ICON_RATIO, BTN_ICON_GAP) } else { (0.0, 0.0) };
        let gl = r.cx() - (isz + gap + tw) * 0.5;
        if let Some(icon) = self.icon {
            crate::ui::icons::draw(p, icon, Rect::new(gl, r.y + (r.h - isz) * 0.5, isz, isz), ink);
        }
        p.text(self.label, gl + isz + gap, ty, self.sz, ink, 0, 1); // left-aligned after the icon
    }
}

// ---- Badge: the small rounded metadata chip (CC / SDH / AD / FORCED / codec tags), with an
// OPTIONAL leading glyph. ONE leaf for the track-menu rows, the Info card meta line, the detail
// About column and the episode filmstrip's duration pill, so the chip look can't drift. Cap-band-
// centred bold CAPTION label; width hugs the label with a floor so short tags (CC) still read as a
// chip. Returns the drawn width so callers can flow chips inline. ----
pub(crate) enum BadgeStyle {
    /// 2px border + knockout interior: border+label in `col`, interior filled `bg` (the surface
    /// behind the chip — keeps the outline clean over a light focus pill or a dark panel).
    Outlined { col: [f32; 4], bg: [f32; 4] },
    /// solid translucent fill ([`theme::BADGE_FILL`]), label in [`theme::TEXT_HEADING`] — the
    /// About column's accessibility chips.
    Filled,
    /// A CAPSULE that rides on ARTWORK: the idle-control pair ([`theme::CONTROL_IDLE_FILL`] face,
    /// [`theme::CONTROL_IDLE_INK`] ink) and fully rounded ends — the same surface
    /// [`watched_badge`]'s disc wears, so a chip and a disc laid over the same still read as one
    /// family. Deliberately NOT [`BadgeStyle::Filled`]: that chip's translucent light fill is
    /// legible in a dark text column and disappears over a bright thumbnail.
    ///
    /// Its ink is NEUTRAL on purpose. Amber (`RESUME_*`) is the app's one watched-STATE hue, and a
    /// chip in that hue over a tile that already carries a state mark would be a second, competing
    /// claim about the same item (see `ui/CLAUDE.md`'s one-vocabulary rule).
    OverArt,
}
/// The chip's height — one band for every style, so a row mixing them stays on one line. Public
/// because a caller that pins a chip to an edge (the episode still's duration pill) needs to know
/// how tall the thing it is placing is.
pub(crate) const BADGE_H: f32 = 34.0;
/// A leading glyph's box, at the label's own type size — a touch over its cap height, the same
/// relationship [`rating_badge`]'s brand mark has to its score. Deliberately smaller than
/// [`Button`]'s `sz * BTN_ICON_RATIO`: this chip is half a button's height, so a button-proportioned
/// glyph would fill it edge to edge.
const BADGE_ICON: f32 = theme::size::CAPTION as f32;
/// Glyph → label air. They are one run, so the tightest rung (as in [`rating_badge`]).
const BADGE_ICON_GAP: f32 = theme::space::XS;

/// pixel width [`badge`] will occupy for `text` (+ `icon`) — the layout companion (e.g. reserving
/// the inline-chip run so a row label elides before it). The icon's band is added OUTSIDE the
/// short-tag floor, so a bare "CC" still measures its minimum and a glyphed chip still fits both.
pub(crate) fn badge_w(text: &str, icon: Option<crate::ui::icons::Icon>) -> f32 {
    const PAD: f32 = 12.0;
    const MIN_W: f32 = 56.0;
    let lead = if icon.is_some() { BADGE_ICON + BADGE_ICON_GAP } else { 0.0 };
    std::ffi::CString::new(text)
        .ok()
        .map(|c| (crate::text::text_width(c.as_ptr(), theme::size::CAPTION, 1) + 2.0 * PAD).max(MIN_W) + lead)
        .unwrap_or(0.0)
}
/// Draw one chip with its LEFT edge at `x`, vertically centred on `cy`; returns its width.
pub(crate) fn badge(p: Painter, x: f32, cy: f32, text: &str, icon: Option<crate::ui::icons::Icon>, style: BadgeStyle) -> f32 {
    let lc = match std::ffi::CString::new(text) {
        Ok(c) => c,
        Err(_) => return 0.0,
    };
    let sz = theme::size::CAPTION;
    let w = badge_w(text, icon);
    let r = Rect::new(x, cy - BADGE_H * 0.5, w, BADGE_H);
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
        BadgeStyle::OverArt => {
            let rad = r.h * 0.5;
            p.rrect(r, rad, rad, theme::CONTROL_IDLE_FILL);
            theme::CONTROL_IDLE_INK
        }
    };
    let ty = crate::text::text_vcenter_y(sz, 1, cy);
    // [glyph + gap + label] centred in the chip as ONE run — the same composition `Button::draw`
    // uses, so a chip and a pill put their icon in the same optical place. With no icon `lead` is 0
    // and this collapses to the label centred on its own, which is what it always did.
    let lead = if icon.is_some() { BADGE_ICON + BADGE_ICON_GAP } else { 0.0 };
    let tw = crate::text::text_width(lc.as_ptr(), sz, 1);
    let gl = r.cx() - (lead + tw) * 0.5;
    if let Some(i) = icon {
        crate::ui::icons::draw(p, i, Rect::new(gl, cy - BADGE_ICON * 0.5, BADGE_ICON, BADGE_ICON), ink);
    }
    p.text(lc.as_ptr(), gl + lead, ty, sz, ink, 0, 1); // left-aligned after the glyph
    w
}

// ---------------------------------------------------------------------------------------
// ---- Rating badge: one review score behind its provider's brand mark. Deliberately NOT a [`badge`]
// chip: a chip's border boxes in art that is already a distinct silhouette, and four boxed chips in a
// row shout over the hero. The score is ordinary primary ink and the pair reads as part of the
// metadata block it sits in. Returns the drawn width so a row can flow badges inline, with
// [`rating_badge_w`] as the measure-first companion (same contract as `badge`/`badge_w`).
//
// A provider's mark takes ONE of two forms, and which one is a property of the brand, not a style
// choice (`Details Screen.dc.html` draws all four):
//
//   * [`RatingMark::Glyph`] — Rotten Tomatoes, whose marks ARE silhouettes (a tomato, a tub) and
//     whose *state* is the drawing. Two stacked masks, base + accent, so the tomato keeps its green
//     leaf and the tub its gold popcorn; see `ui/icons.rs`.
//   * [`RatingMark::Wordmark`] — IMDb and TMDB, whose brands are LOGOTYPES. Their old silhouettes (a
//     generic star, a generic ring) named no provider: a five-point star says "a rating", not "IMDb".
//     A gold `IMDb` chip and a teal `TMDB` chip are unmistakable, and cost nothing but a rect.
// ----
/// Mark box (px) for a [`RatingMark::Glyph`]. A little over the meta line's cap height so a 24-unit
/// silhouette still resolves at couch distance — these marks carry the VERDICT (fresh vs rotten), so
/// legibility here is not cosmetic.
const RATING_MARK_D: f32 = 30.0;
/// Glyph mark → score gap. They are one unit, so it stays tight.
const RATING_GAP: f32 = 10.0;
/// Wordmark chip → score gap — one step wider than a glyph's. The chip carries its own side padding,
/// so at an equal geometric gap its INK sits further from the score than a glyph's does; this is the
/// optical correction, and it is why the two are separate constants rather than one shared rung.
const CHIP_SCORE_GAP: f32 = 12.0;
/// The wordmark chip's height — the SAME band as [`BADGE_H`], so a logotype chip and a metadata chip
/// laid on one line agree. (28 first, which set the logotypes two rungs under the score beside them and
/// read as a footnote rather than a brand.)
const CHIP_H: f32 = BADGE_H;
/// Strips a [`Wordmark::face_to`] sweep is banded into. 16 across a ~62px chip is ~4px a band, which
/// is under the eye's threshold for these two stops; the reason it is banded at all is that the
/// painter's only gradient runs VERTICALLY (`Painter::rect`), and this sweep runs left→right.
const SWEEP_BANDS: usize = 16;

/// One provider's logotype, in its own brand colours — the data half of [`RatingMark::Wordmark`].
/// A `static` per provider (see [`WORDMARK_IMDB`]/[`WORDMARK_TMDB`]) rather than fields threaded
/// through the draw call, so a brand's colours are spelled once and the row cannot mix them up.
pub(crate) struct Wordmark {
    /// The brand's name, exactly as the brand sets it (`IMDb`, not `IMDB`).
    pub(crate) text: &'static std::ffi::CStr,
    /// Type size for the logotype. Per-brand, not one shared rung: a longer word needs a step down to
    /// keep its chip from outgrowing its neighbours (`TMDB` is set under `IMDb` for exactly that).
    pub(crate) sz: std::os::raw::c_int,
    /// Face fill, or the START stop when [`Wordmark::face_to`] is set.
    pub(crate) face: [f32; 4],
    /// `Some(stop)` = a left→right two-stop sweep from [`Wordmark::face`] to `stop`; `None` = flat.
    pub(crate) face_to: Option<[f32; 4]>,
    /// Ink over the face.
    pub(crate) ink: [f32; 4],
    /// Corner radius, or `0.0` for a full pill (radius = height/2).
    pub(crate) radius: f32,
    /// Face padding either side of the logotype.
    pub(crate) pad: f32,
    /// Inset border width in the ink colour; `0.0` = none.
    pub(crate) border: f32,
}

/// IMDb: black-bordered gold box. The border is part of the logo, not a chip convention — IMDb sets
/// its wordmark in a keyline box and it looks wrong without one.
pub(crate) static WORDMARK_IMDB: Wordmark = Wordmark {
    text: c"IMDb",
    sz: theme::size::CAPTION,
    face: theme::RATING_IMDB,
    face_to: None,
    ink: theme::RATING_IMDB_INK,
    radius: 6.0,
    pad: 10.0,
    border: 2.5,
};
/// TMDB: a pill carrying its green→blue brand sweep, no keyline.
pub(crate) static WORDMARK_TMDB: Wordmark = Wordmark {
    text: c"TMDB",
    sz: theme::size::MICRO,
    face: theme::RATING_TMDB_LO,
    face_to: Some(theme::RATING_TMDB),
    ink: theme::RATING_TMDB_INK,
    radius: 0.0,
    pad: 12.0,
    border: 0.0,
};

/// One colour layer of a [`RatingMark::Glyph`] — a mask and the tint it is painted in.
pub(crate) type MarkLayer = (crate::ui::icons::Icon, [f32; 4]);

/// How one provider draws its mark — see the block comment above for why the two forms exist.
pub(crate) enum RatingMark {
    /// Tinted masks painted back-to-front at the SAME rect, one per colour. A slice rather than a
    /// base-plus-accent pair because Certified Fresh needs three (a gold seal, a red box, and green
    /// calyx-and-banner over both) and a plain tomato needs two — and because the ORDER is the
    /// drawing: the layers overlap, so back-to-front is load-bearing, not incidental.
    ///
    /// Each layer must be a single `<path>` (or a single primitive element); `ui/icons.rs`'s
    /// authoring contract explains why — two elements inside ONE layer alpha-composite and leave a
    /// visible crease where their antialiased edges meet. Across layers that compositing is exactly
    /// what draws the mark.
    Glyph(&'static [MarkLayer]),
    /// The provider's logotype in its own chip.
    Wordmark(&'static Wordmark),
}

impl RatingMark {
    /// The mark's own width — everything left of the score's gap.
    fn width(&self) -> f32 {
        match self {
            RatingMark::Glyph(_) => RATING_MARK_D,
            RatingMark::Wordmark(w) => wordmark_chip_w(w),
        }
    }
    /// Mark → score gap, which differs by form (see [`CHIP_SCORE_GAP`]).
    fn gap(&self) -> f32 {
        match self {
            RatingMark::Glyph(_) => RATING_GAP,
            RatingMark::Wordmark(_) => CHIP_SCORE_GAP,
        }
    }
}

/// The chip's drawn width for `w`'s logotype — the layout companion to [`wordmark_chip`].
pub(crate) fn wordmark_chip_w(w: &Wordmark) -> f32 {
    crate::text::text_width(w.text.as_ptr(), w.sz, 1) + 2.0 * w.pad
}

/// Draw one provider's logotype chip with its LEFT edge at `x`, centred on `cy`; returns its width.
/// Public because it is a brand mark, not a rating-row internal — anything that needs to say "IMDb"
/// should say it this way rather than re-styling a [`badge`].
pub(crate) fn wordmark_chip(p: Painter, x: f32, cy: f32, w: &Wordmark) -> f32 {
    let cw = wordmark_chip_w(w);
    let r = Rect::new(x, cy - CHIP_H * 0.5, cw, CHIP_H);
    let rad = if w.radius > 0.0 { w.radius } else { CHIP_H * 0.5 };
    match w.face_to {
        // A left→right sweep, banded: clip to a vertical slice and fill the WHOLE chip-shaped
        // rounded rect in that band's colour, so every band inherits the chip's silhouette and the
        // corner arcs survive. Same scissor trick as `card_row::resume_bar`'s corner-wrapping fill.
        Some(to) => {
            let bw = r.w / SWEEP_BANDS as f32;
            for i in 0..SWEEP_BANDS {
                let t = (i as f32 + 0.5) / SWEEP_BANDS as f32;
                let c = [
                    w.face[0] + (to[0] - w.face[0]) * t,
                    w.face[1] + (to[1] - w.face[1]) * t,
                    w.face[2] + (to[2] - w.face[2]) * t,
                    w.face[3] + (to[3] - w.face[3]) * t,
                ];
                // +1px of overlap: the scissor is integer, so abutting bands can leave a seam.
                p.clip(Rect::new(r.x + i as f32 * bw, r.y, bw + 1.0, r.h));
                p.rrect(r, rad, rad, c);
            }
            p.clip_clear();
        }
        None => p.rrect(r, rad, rad, w.face),
    }
    if w.border > 0.0 {
        // knockout: the keyline in ink, then the face inset by it — the same two-call shape
        // `BadgeStyle::Outlined` uses, in the opposite order (ink outside, face inside).
        p.rrect(r, rad, rad, w.ink);
        let b = w.border;
        p.rrect(
            Rect::new(r.x + b, r.y + b, r.w - 2.0 * b, r.h - 2.0 * b),
            (rad - b).max(0.0),
            (rad - b).max(0.0),
            w.face,
        );
    }
    let ty = crate::text::text_vcenter_y(w.sz, 1, cy);
    p.text(w.text.as_ptr(), r.x + w.pad, ty, w.sz, w.ink, 0, 1);
    cw
}

/// pixel width [`rating_badge`] will occupy — measure before drawing so a row can stop at a margin
/// instead of running a badge off the panel.
pub(crate) fn rating_badge_w(mark: &RatingMark, value: &str) -> f32 {
    std::ffi::CString::new(value)
        .ok()
        .map(|c| {
            mark.width() + mark.gap() + crate::text::text_width(c.as_ptr(), theme::size::LABEL, 1)
        })
        .unwrap_or(0.0)
}

/// Draw one rating badge with its LEFT edge at `x`, centred on `cy`; returns its width.
pub(crate) fn rating_badge(p: Painter, x: f32, cy: f32, mark: &RatingMark, value: &str) -> f32 {
    let vc = match std::ffi::CString::new(value) {
        Ok(c) => c,
        Err(_) => return 0.0,
    };
    match mark {
        RatingMark::Glyph(layers) => {
            // every layer rides the SAME rect, so all of them rasterize at one size from one 24×24
            // viewBox and register exactly — see `ui/icons.rs`'s note on the layered marks
            let r = Rect::new(x, cy - RATING_MARK_D * 0.5, RATING_MARK_D, RATING_MARK_D);
            for (mask, tint) in layers.iter() {
                crate::ui::icons::draw(p, *mask, r, *tint);
            }
        }
        RatingMark::Wordmark(w) => {
            wordmark_chip(p, x, cy, w);
        }
    }
    let ty = crate::text::text_vcenter_y(theme::size::LABEL, 1, cy);
    p.text(
        vc.as_ptr(),
        x + mark.width() + mark.gap(),
        ty,
        theme::size::LABEL,
        theme::TEXT_PRIMARY,
        0,
        1,
    );
    // the MEASURED width, not what `text` reports it rasterized — `badge` calls `badge_w` for the
    // same reason: a draw and its measurer that can disagree will eventually be caught disagreeing
    rating_badge_w(mark, value)
}


#[cfg(test)]
mod tests {
    use super::*;

    /// A spread of pill widths. Deliberately wider than the device's (a real "TV" pill measures
    /// ≈67px at `size::BODY`, "Kids & Family Movies" ≈350) so a modest pill count crosses
    /// [`TAB_VIEW_MAX`] and the overflow arithmetic is exercised without inventing 30 libraries —
    /// the thresholds below are therefore about the geometry, not about a particular server.
    fn widths_for(n: usize) -> Vec<f32> {
        (0..n).map(|i| 140.0 + (i % 5) as f32 * 60.0).collect()
    }

    /// The unit-10 invariant, stated the way the [`tab_count`] doc states it: EVERY pill can be
    /// drawn, because the strip scrolls to it. For any section count, and from any starting
    /// scroll, the target for pill `i` puts that whole pill inside the visible strip — so no
    /// focusable index can lack a drawn rect, which is what the old `MAX_TABS` cap used to buy
    /// by refusing to focus the pills it could not reach.
    #[test]
    fn every_pill_scrolls_fully_into_the_strips_viewport() {
        for n in 1..=16usize {
            let w = widths_for(n);
            let view_w = tab_view_w(&w);
            let max = (tab_content_w(&w) - view_w).max(0.0);
            for &start in &[0.0f32, 400.0, -900.0, 9000.0] {
                for i in 0..n {
                    let t = tab_scroll_target(&w, i, start);
                    assert!(t >= -0.01 && t <= max + 0.01, "n={n} i={i}: scroll {t} outside 0..={max}");
                    let x = tab_pill_x(&w, i) - t; // the pill's left edge in viewport space
                    assert!(
                        x >= -0.01 && x + w[i] <= view_w + 0.01,
                        "n={n} i={i} (start {start}): pill spans {x}..{} of a {view_w}-wide strip",
                        x + w[i]
                    );
                }
            }
        }
    }

    /// A row that fits must keep its centered, unscrolled tvOS look — the track IS the content and
    /// there is nowhere to scroll to. Past that the scroll is minimal, not eager: asking for a
    /// pill that is already on screen must leave the strip exactly where it is, wherever that is.
    #[test]
    fn the_strip_scrolls_only_once_it_outgrows_the_row_and_only_as_far_as_it_must() {
        let fits = widths_for(4);
        assert!(tab_content_w(&fits) <= TAB_VIEW_MAX, "4 pills of this size must still fit");
        assert_eq!(tab_view_w(&fits), tab_content_w(&fits), "the track is the content");
        assert_eq!(tab_content_w(&fits) - tab_view_w(&fits), 0.0, "…so there is no scroll range");

        let over = widths_for(12);
        assert!(tab_content_w(&over) > TAB_VIEW_MAX, "12 pills of this size must overflow");
        assert_eq!(tab_view_w(&over), TAB_VIEW_MAX, "the track caps at the viewport");
        assert!(tab_scroll_target(&over, 11, 0.0) > 0.0, "the last pill must be scrolled to");
        assert_eq!(tab_scroll_target(&over, 0, 0.0), 0.0, "the first pill is already at the start");
        assert_eq!(tab_scroll_target(&over, 1, 0.0), 0.0, "a pill in view does not move the strip");
        // and the same rule part-way along: pill 5 sits inside the viewport at scroll 800, so the
        // strip HOLDS there rather than re-centering on it
        assert_eq!(tab_scroll_target(&over, 5, 800.0), 800.0, "a mid-strip pill in view holds");
    }

    /// A focus index left over from a bigger section table (the table is refetched on sign-in or
    /// a server switch) must clamp, not index out of bounds — the strip is drawn from these same
    /// widths every frame, so a panic here would be a panic in the frame loop. Graded on an
    /// OVERFLOWING row so the clamp has a non-zero range to be wrong about.
    #[test]
    fn a_stale_pill_index_clamps_instead_of_panicking() {
        let w = widths_for(12);
        let max = tab_content_w(&w) - tab_view_w(&w);
        assert!(max > 0.0, "the fixture must actually have somewhere to scroll");
        assert_eq!(tab_scroll_target(&w, 99, 500.0), 500.0, "an in-range scroll is left alone");
        assert_eq!(tab_scroll_target(&w, 99, 9e3), max, "an over-scroll is pulled back to the end");
        assert_eq!(tab_scroll_target(&w, 99, -5.0), 0.0, "a negative scroll is pulled to the start");
        assert_eq!(tab_pill_x(&w, 99), tab_pill_x(&w, 12), "past the end reads as the strip's end");
        assert_eq!(tab_content_w(&[]), 0.0, "an empty strip has no width and no gaps");
        assert_eq!(tab_scroll_target(&[], 0, 12.0), 0.0);
    }
}
