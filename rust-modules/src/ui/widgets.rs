//! Reusable retui leaves + shared helpers. These are the "reusable UI elements":
//! Button, CircleButton, TabPill, TransportButton, PageDots, Badge, plus the shared art-card
//! core (`card`/`draw_card`) and the poster-resolve helper. (Multi-line text wrapping
//! now lives in the `TextView` primitive in `text_view.rs`.)
use crate::plex::ServerId;
use crate::pms::PmsMovie;
use crate::ui::theme;
use crate::ui::label::{HAlign, Label};
use crate::ui::{Env, Painter, Rect, Spring, View};
use std::ffi::CString;
use std::os::raw::{c_char, c_int};

/// Build `srv`'s transcode key for `path` on the stack. The ONE key builder the resolvers below
/// share, so a warm and the draw that follows it can never name two different slots — `(server,
/// path, w, h, png)` IS the store key. Per-frame hot path (every visible tile): the
/// NUL-terminated copy `poster_key` wants is made on the stack, no heap alloc.
fn tex_key(srv: ServerId, path: &str, w: c_int, h: c_int, png: c_int) -> [u8; 352] {
    let mut p = [0u8; 256];
    crate::cbuf::set_bytes(&mut p, path);
    let mut key = [0u8; 352];
    crate::posters::poster_key(srv, key.as_mut_ptr() as *mut c_char, key.len(), p.as_ptr() as *const c_char, w, h, png);
    key
}

/// build the transcode key on the stack and resolve it to a GL texture (0 until loaded), for art
/// on the server the user is browsing.
///
/// **A thumb path is only meaningful on the server that issued it** — rating keys are server-local
/// integers from 1, so the same `/library/metadata/42/thumb/…` names a different film on a
/// friend's share. This form says "the current server", which is the right answer for art that
/// belongs to the screen (a section's own tiles, a person's headshot) and the wrong one for an
/// item that came from somewhere else: those call [`resolve_tex_on`] with the item's own server.
pub(crate) fn resolve_tex(path: &str, w: c_int, h: c_int, png: c_int) -> u32 {
    resolve_tex_on(crate::plex::current_server(), path, w, h, png)
}

/// [`resolve_tex`] for art belonging to a NAMED server.
pub(crate) fn resolve_tex_on(srv: ServerId, path: &str, w: c_int, h: c_int, png: c_int) -> u32 {
    if path.is_empty() {
        return 0;
    }
    crate::posters::poster_get(srv, tex_key(srv, path, w, h, png).as_ptr() as *const c_char)
}

/// [`resolve_tex`] plus the DECODED pixel size of the texture — `(0, 0.0, 0.0)` until it is ready.
/// For art that must be FIT or COVERED into its frame rather than stretched to it
/// ([`Rect::cover`](crate::ui::Rect::cover)). The size is the store's own answer about the slot it
/// just probed for the texture, so this is the SAME single lookup [`resolve_tex`] does — knowing the
/// source aspect is free, and a screen never pays a second lock + key scan for it.
pub(crate) fn resolve_tex_wh(path: &str, w: c_int, h: c_int, png: c_int) -> (u32, f32, f32) {
    resolve_tex_wh_on(crate::plex::current_server(), path, w, h, png)
}

/// [`resolve_tex_wh`] for art belonging to a NAMED server.
pub(crate) fn resolve_tex_wh_on(srv: ServerId, path: &str, w: c_int, h: c_int, png: c_int) -> (u32, f32, f32) {
    if path.is_empty() {
        return (0, 0.0, 0.0);
    }
    let key = tex_key(srv, path, w, h, png);
    let (tex, pw, ph) = crate::posters::poster_get_wh(srv, key.as_ptr() as *const c_char);
    (tex, pw as f32, ph as f32)
}

/// The prefetch twin of [`resolve_tex`]: build the same transcode key and hand it to
/// [`posters::poster_warm`](crate::posters::poster_warm) — start the fetch, take no texture, take no
/// LRU protection. Same arguments on purpose, so a screen warms EXACTLY the key it will later
/// resolve; a warm at a different size — or on a different server — is a different slot and buys
/// nothing.
pub(crate) fn warm_tex(path: &str, w: c_int, h: c_int, png: c_int) -> crate::posters::Warm {
    warm_tex_on(crate::plex::current_server(), path, w, h, png)
}

/// [`warm_tex`] for art belonging to a NAMED server.
pub(crate) fn warm_tex_on(srv: ServerId, path: &str, w: c_int, h: c_int, png: c_int) -> crate::posters::Warm {
    if path.is_empty() {
        return crate::posters::Warm::Known;
    }
    let key = tex_key(srv, path, w, h, png);
    crate::posters::poster_warm(srv, key.as_ptr() as *const c_char)
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
            // The ONE state language on every poster, drawn in this shared composite so Home
            // shelves + the Library grid + Related all inherit it: the amber WATCHED disc here
            // (finished), the amber resume BAR (`card_row::resume_bar`) for in progress, and —
            // deliberately — NOTHING for never started.
            //
            // **Amber means "you have watched this"**, one hue for one vocabulary. Until 2026-08-13
            // this corner carried the opposite claim (an amber ANGLE marking a fully UNWATCHED
            // item), and the inversion is the design system's (`ArtTile`: "most of the server is
            // unwatched, so only a finished tile is marked — a bare tile means nothing has been
            // seen"). The old polarity made a freshly-added library a wall of amber where the mark
            // said nothing you could act on, and left the one item you had actually finished as the
            // only clean tile on the shelf; it also made the poster disagree with the episode
            // still beside it, whose `✓` has always meant watched.
            //
            // Never both marks. **In progress WINS over watched**, because PMS keeps a resume point
            // on a finished-then-restarted item, so the wire says both — and being part-way through
            // a re-watch is what the viewer is actually doing (`detail::ep_state` resolves the same
            // three states for a still, and its table is the authority for all of them).
            if let Some(m) = m {
                if poster_mark(m) == PosterMark::Watched {
                    watched_mark(p, r, rad);
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

/// Which state mark a poster wears — the pure half of [`card`]'s corner, split out for exactly the
/// reason `detail::ep_state` is: the CHOICE is the behaviour, while drawing it needs a GL context no
/// host test has. Nothing else inside `card` is assertable, and this is the part that can be wrong.
///
/// | state | mark | drawn by |
/// |---|---|---|
/// | never started | nothing — and most of a server is here | — |
/// | in progress | the full-bleed resume BAR | the CALLER (`card_row::draw_tile`/`draw_focused`) |
/// | watched | the amber corner DISC | [`card`] |
///
/// [`PosterMark::InProgress`] is *defined* as "[`PmsMovie::resume_frac`] has a value" — precisely
/// when the caller draws the bar — so the two halves cannot disagree and put two marks on one tile.
/// That is also the precedence: a re-watch in flight outranks the watched flag, because PMS reports
/// both on a finished-then-restarted item and being part-way through the re-watch is what the viewer
/// is doing. Same answer as the still's resolver, on the same item.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PosterMark {
    None,
    InProgress,
    Watched,
}

/// Resolve [`PosterMark`] from a catalog row. Total and mutually exclusive by construction.
///
/// Keyed on `PmsMovie::watched` — **not** on `!unwatched`, which is a weaker claim for a container:
/// a SHOW with one episode played is `!unwatched` but nowhere near done, and marking it watched is a
/// statement the viewer can see is false. Partly-watched shows therefore land on [`PosterMark::None`]
/// beside never-started ones; the true statement about a series mid-run is where its next episode
/// stands, which a poster in a grid is not the place for.
pub(crate) fn poster_mark(m: &PmsMovie) -> PosterMark {
    if m.resume_frac().is_some() {
        return PosterMark::InProgress;
    }
    if m.watched {
        PosterMark::Watched
    } else {
        PosterMark::None
    }
}

/// The **watched tick** on a poster, as fractions of the tile's DRAWN width: the tick's box, its
/// corner inset, then the veil's box. Anchored on the design system's `ArtTile` — a 26px tick inset
/// 12px under a 104px corner veil, on a 250-wide poster — and held as ratios rather than pixels so
/// the whole mark rides the focus pop as one object (a fixed 26px tick inside a tile growing to 1.09
/// visibly shrinks and drifts inward). The component's second rung (20px/76px on a tile under 200
/// wide) has no poster in this product: every poster shelf, the Library grid and Related are all
/// `consts::CARD_W` 250.
const TICK_RATIO: f32 = 26.0 / 250.0;
const TICK_INSET: f32 = 12.0 / 250.0;
const VEIL_RATIO: f32 = 104.0 / 250.0;
/// The tick's drop shadow, as fractions of the TICK's box (the design's `0 2px 7px`).
const TICK_SHADOW_BLUR: f32 = 7.0 / 26.0;
const TICK_SHADOW_DY: f32 = 2.0 / 26.0;
/// Where the veil's falloff reaches zero, as a fraction of its box — CSS
/// `radial-gradient(72% 72% at 100% 0%, veil, transparent 70%)` resolves to `.72 × .70`.
const VEIL_EXTENT: f32 = 0.72 * 0.70;
/// The veil texture's resolution. A power of two (NPOT sampling is a documented Mali trap) and far
/// finer than the ~52px of falloff it is stretched across, so the ramp is smooth under GL_LINEAR.
const VEIL_TEX_PX: usize = 64;
static mut VEIL_TEX: std::os::raw::c_uint = 0;

/// The corner **veil** texture: white RGB with a radial alpha falloff peaking at the TOP-RIGHT
/// corner, generated once and reused at every size. It is a texture rather than geometry because the
/// renderer has no radial gradient, and the two alternatives both fail on this shape: a `grad4` quad
/// is bilinear and, worse, square — it would spill past the tile's 14px corner ARC onto the shelf at
/// full strength, exactly where the mark is strongest; and stepping it as N rounded-rect bands (the
/// `art_scrim` trick) is what `hero_scrim`'s doc already rejected for a field this wide, at a visible
/// alpha staircase with `GL_DITHER` off. `Painter::tex` takes a corner radius, so ONE draw of this
/// gets the tile's own silhouette for free — the veil's other three corners live in fully
/// transparent territory, so rounding them changes nothing.
fn veil_tex() -> std::os::raw::c_uint {
    unsafe {
        let cached = *std::ptr::addr_of!(VEIL_TEX);
        if cached != 0 {
            return cached;
        }
        let n = VEIL_TEX_PX;
        let mut px = vec![0u8; n * n * 4];
        for y in 0..n {
            for x in 0..n {
                // distance from the top-right corner, in units of the box's width
                let dx = (n - 1 - x) as f32 / (n - 1) as f32;
                let dy = y as f32 / (n - 1) as f32;
                let a = (1.0 - (dx * dx + dy * dy).sqrt() / VEIL_EXTENT).clamp(0.0, 1.0);
                let i = (y * n + x) * 4;
                px[i] = 255;
                px[i + 1] = 255;
                px[i + 2] = 255;
                px[i + 3] = (a * 255.0).round() as u8;
            }
        }
        let tex = crate::gfx::upload_rgba(0, n as std::os::raw::c_int, n as std::os::raw::c_int, px.as_ptr());
        *std::ptr::addr_of_mut!(VEIL_TEX) = tex;
        tex
    }
}

/// The **watched** mark on a poster: a corner veil, then a bare tick over it. `card` is the rect
/// actually drawn (the SCALED one while the tile is popped) and `rad` its corner radius, which the
/// veil is masked to so nothing lands outside the tile's own silhouette.
///
/// No disc and no plate — the artwork stays visible and only the falloff touches it. The white tick
/// carries no contrast of its own, so legibility is the veil's job with the tick's own soft shadow
/// inside it; between them the mark holds on a snowfield and on a black-and-white title card, which
/// is the case that decided against a bare tick alone.
pub(crate) fn watched_mark(p: Painter, card: Rect, rad: f32) {
    let v = card.w * VEIL_RATIO;
    p.tex(veil_tex(), Rect::new(card.x + card.w - v, card.y, v, v), rad, theme::TILE_MARK_VEIL);
    // Quantized to 4px so the focus pop reuses a handful of cached masks instead of rasterizing +
    // uploading one per rounded pixel, and proportional to the DRAWN tile so the mark rides the pop.
    let d = ((card.w * TICK_RATIO) / 4.0).round() * 4.0; // 24px on a 250 card (26 → nearest rung)
    let ins = card.w * TICK_INSET;
    let tick = Rect::new(card.x + card.w - ins - d, card.y + ins, d, d);
    p.shadow(tick, d * 0.5, d * TICK_SHADOW_BLUR, d * TICK_SHADOW_DY, theme::TILE_MARK_SHADOW);
    crate::ui::icons::draw(p, crate::ui::icons::Icon::Check, tick, theme::TILE_MARK_INK);
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
/// The chip's label weight — the mock's `font-weight:600`. See [`keyline_chip`] for why it is bold.
const KEYLINE_BOLD: std::os::raw::c_int = 1;

/// The width [`keyline_chip`] will occupy for `text` — the measure-first companion.
pub(crate) fn keyline_chip_w(text: &str) -> f32 {
    std::ffi::CString::new(text)
        .ok()
        .map(|c| crate::text::text_width(c.as_ptr(), theme::size::CAPTION, KEYLINE_BOLD) + 2.0 * KEYLINE_PAD_X)
        .unwrap_or(0.0)
}

/// Draw a fine-print keyline chip with its LEFT edge at `x`, centred on `cy`; returns its width.
///
/// **A real hollow ring** ([`Painter::rring`]), so whatever is behind it shows through the middle —
/// the mock's `box-shadow: inset 0 0 0 1.5px …` with no `background`. It used to be a KNOCKOUT
/// (stroke colour, then the interior repainted in a `bg` the caller named), which is exact on a
/// flat panel and wrong over artwork: on the detail hero's identity line the ground is a backdrop
/// plus two scrim ramps, so a chip claiming `SURFACE_APP` read as a dark box over a bright still
/// instead of a hairline. The parameter is gone rather than defaulted — there was no honest value
/// for it, which is the point.
/// The ring takes the LABEL'S OWN INK, and the mock's `rgba(255,255,255,.34)` deliberately does
/// not port — it cannot. `Painter::rring` is the SDF's rim band, whose coverage is the product of
/// two 1.5px-wide smoothsteps (`fs_src.frag`); at [`KEYLINE_W`] the window where both reach 1 is
/// about a QUARTER of a pixel, so no pixel centre lands in it and a thin rim never resolves to
/// more than a fraction of its stated alpha. Measured on the panel: `white .34` came out ~12
/// levels above the ground where the arithmetic says ~87 — an outline nobody can see. The opaque
/// ink is what makes a 1.5px ring exist at all here, and it is what shipped before this was
/// briefly "corrected" to the mock's literal value.
///
/// The label is BOLD for the same reason the mock sets `font-weight:600` on it: two or three caps
/// at `CAPTION` inside a ring have to hold their own against it, and regular weight is what made
/// this chip read as an empty frame in the first device photograph of the identity line.
pub(crate) fn keyline_chip(p: Painter, x: f32, cy: f32, text: &str, col: [f32; 4]) -> f32 {
    let lc = match std::ffi::CString::new(text) {
        Ok(c) => c,
        Err(_) => return 0.0,
    };
    let w = keyline_chip_w(text);
    let h = crate::text::cap_h(theme::size::CAPTION, KEYLINE_BOLD) + 2.0 * KEYLINE_PAD_Y; // hugs the label's cap band, not a fixed band
    p.rring(Rect::new(x, cy - h * 0.5, w, h), KEYLINE_RAD, KEYLINE_W, col);
    p.text(
        lc.as_ptr(),
        x + KEYLINE_PAD_X,
        crate::text::text_vcenter_y(theme::size::CAPTION, KEYLINE_BOLD, cy),
        theme::size::CAPTION,
        col,
        0,
        KEYLINE_BOLD,
    );
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

// ---- The hero corner scrim: the wedge that makes hero copy legible over ARTWORK ---------------
//
// A **sibling** of `art_scrim`, deliberately not a direction flag on it. `art_scrim`'s entire body
// is the scissor-vs-fill seam problem on a ROUNDED CARD — snapped bands, `SCRIM_CORNER_BANDS`,
// clip/clip_clear — and a full-bleed hero has no corner arcs, no scissor and a different axis.
// Folding a flag into it would make that seam machinery conditional on a case it never runs in,
// which is forking by another name. What the two do share is the rule: a label sits directly on
// artwork only where something has bought it the contrast to.

/// How far the hero wedge reaches before it is gone entirely: it peaks at x=0 and is exactly 0
/// here. Four fifths of the panel, because it has to still be carrying weight under the RIGHT END
/// of the longest lines, not just at the margin — detail's synopsis column runs to x=990
/// (`MARGIN_X` + its `HERO_TEXT_W`) and its facts line to ~1270. A tighter falloff strands exactly
/// the ends of the lines a long title or blurb produces. The last fifth of the frame is left
/// completely alone: that is the side of the picture the composition is usually about.
const HERO_SCRIM_W: f32 = 0.80 * crate::ui::consts::SCR_W; // 1536

/// Where the wedge starts feathering in. Clears the tab track's own band ([`TOP_BAR_Y`] 44 +
/// [`TAB_PILL_H`] 60 + [`TAB_TRACK_PAD`] 8 = 112) by 50px: the top chrome owns its legibility with
/// its own dark capsule (`draw_tab_row`) and must not get a second treatment stacked under it.
const HERO_SCRIM_TOP: f32 = 0.15 * crate::ui::consts::SCR_H; // 162
/// Where the wedge reaches full strength — and the seam between its two quads, named once so the
/// pair is watertight by construction. It sits above every hero TEXT anchor, which is what lets
/// [`hero_scrim_a`] be a function of x alone: both heroes baseline their title on the bottom of a
/// `hero_logo::band_h` band (home's stack bottoms out at y≈528, detail's on `TITLE_BOTTOM` 566), so
/// the highest cap top on either screen is ~479, and every line below it is lower still.
///
/// What DOES cross this line is a clearLogo, which is art: the band is a layout floor and a squarer
/// mark spills upward out of it as paint, to y=298 on detail and y=260 on home. Up there the wedge
/// is still feathering in (~0.27 of its peak at y=260) rather than absent, which is the reason nit
/// 2's taller logos were sequenced AFTER this component; the clearance that binds them is the top
/// chrome's ([`TOP_BAR_BOTTOM`]), asserted in `home.rs`, not this knee.
const HERO_SCRIM_KNEE: f32 = 0.39 * crate::ui::consts::SCR_H; // 421.2

/// The mirrored RIGHT wedge — for a hero with a right-aligned column, which today means detail's
/// "Starring" block at x 1270..1830, the one piece of hero copy the left wedge by definition cannot
/// reach. Weaker and shorter than the left one because the bottom-up ramp is already ~0.65 across
/// that band: this closes the gap, it does not carry the load. It is also the component's banding
/// risk (`GL_DITHER` is deliberately off) — ~1 8-bit code per 4–5px near its origin. If it bands,
/// make these SMALLER (steeper = fewer codes per pixel); never re-enable dithering.
const HERO_SCRIM_R_W: f32 = 700.0;
const HERO_SCRIM_R_TOP: f32 = 0.65 * crate::ui::consts::SCR_H; // 702
const HERO_SCRIM_R_A: f32 = 0.50;

/// Where the frame-wide ATMOSPHERIC ramp starts — the treatment's other half, which both heroes
/// paint under this wedge (`home::Backdrop::draw` / `detail::draw_backdrop`): nothing above this
/// line, running to the foot of the panel. It sits here with the wedge's own stops because it is
/// one treatment's first stop, and it was declared verbatim, under the same name, in two screens.
///
/// The two screens' CURVES below it are deliberately **not** unified — home's is a two-stop ramp
/// with a midpoint knee, detail's a single linear stop, and they land within ~0.05 alpha of each
/// other everywhere. Retuning the atmospheric floor is a different decision from sharing its
/// origin, and only the panel can judge it; the curves stay as `home::base_scrim_a` /
/// `detail::base_scrim_a`, which is also what the legibility table below grades.
pub(crate) const HERO_BASE_SCRIM_Y0: f32 = 0.34 * crate::ui::consts::SCR_H; // 367.2

/// The hero wedge's alpha at `x`, at or below [`HERO_SCRIM_KNEE`] — which is where every hero text
/// anchor sits, by construction (see that const). Pure, because the legibility contract is graded
/// on it: this is the arithmetic the anchor table in this module's tests reads, so the promise and
/// the paint cannot come from two different curves.
///
/// `strength` is the screen's own hero fade (home's `env.hero_a`, detail's
/// `hero_alpha(scroll, HERO_FADE)`) — everything scales by it, so the wedge leaves with the hero
/// rather than lingering over the shelves, where the flat ground is what makes card shadows read.
pub(crate) fn hero_scrim_a(x: f32, strength: f32) -> f32 {
    let u = (x.max(0.0) / HERO_SCRIM_W).min(1.0);
    theme::SCRIM_TEXT_A * (1.0 - u) * strength.clamp(0.0, 1.0)
}

/// The mirrored right wedge's alpha at `(x, y)` — [`hero_scrim_a`]'s sibling, and the only part of
/// the field that is two-dimensional, since it feathers in from its left edge AND from its top and
/// peaks only in the bottom-right corner. Pure for the same reason.
pub(crate) fn hero_scrim_right_a(x: f32, y: f32, strength: f32) -> f32 {
    let u = ((x - (crate::ui::consts::SCR_W - HERO_SCRIM_R_W)).max(0.0) / HERO_SCRIM_R_W).min(1.0);
    let v = ((y - HERO_SCRIM_R_TOP).max(0.0) / (crate::ui::consts::SCR_H - HERO_SCRIM_R_TOP)).min(1.0);
    HERO_SCRIM_R_A * strength.clamp(0.0, 1.0) * u * v
}

/// The wedge's quads as `(rect, [tl, tr, br, bl])`, built pure so the seam between quad 0 and
/// quad 1 — the one structural bug this component can have — is host-gradeable. `n` is how many
/// entries are live: 2, or 3 when `right`.
///
/// The whole field is **one ink at four alphas** ([`theme::scrim`]), which is [`Painter::grad4`]'s
/// stated precondition: straight (non-premultiplied) rgba only interpolates exactly across a quad
/// when the corners share an rgb.
///
/// | # | rect | field it produces |
/// |---|---|---|
/// | 0 | `(0, TOP, W, KNEE−TOP)` | feather-in: 0 along the whole top edge, the full wedge along the bottom |
/// | 1 | `(0, KNEE, W, SCR_H−KNEE)` | constant in y → a pure horizontal ramp to the frame's foot |
/// | 2 | `(SCR_W−R_W, R_TOP, R_W, SCR_H−R_TOP)` | feathers in from the top AND the left, peaking bottom-right |
///
/// **Quad 0 and quad 1 abut exactly**, and must keep doing so: quad 0's bottom pair (`bl→br` =
/// edge→none) is identical to quad 1's top pair (`tl→tr` = edge→none) at every x, and the two share
/// one float y. The reflex here is to reach for [`crate::gfx::snap`] — don't. [`art_scrim`] snaps
/// because an integer-truncated *scissor* meets a float *fill*; these are fill-to-fill quads
/// sharing an edge, where the rasterizer's own fill rule already guarantees neither a gap (one row
/// of unscrimmed BRIGHT artwork) nor a double-cover (one row of doubled scrim). Snapping would be
/// cargo cult, and it would move the seam off the shared float.
///
/// **The corners are the closed forms EVALUATED AT THE CORNERS**, not a second hand-written copy of
/// them: a quad's corner alpha is exactly what [`hero_scrim_a`] / [`hero_scrim_right_a`] say at that
/// `(x, y)`, so at the corners the field the legibility contract is graded on and the field that is
/// painted cannot drift — the tests below no longer assert that agreement, only the part
/// construction cannot pin (that the closed forms are affine / bilinear in BETWEEN the corners,
/// which is the shape `grad4` can actually interpolate). It also gives the two closed forms the
/// production caller they otherwise lacked. Both clamp `strength` themselves, so it is passed raw.
pub(crate) fn hero_scrim_quads(strength: f32, right: bool) -> ([(Rect, [[f32; 4]; 4]); 3], usize) {
    let (sw, sh) = (crate::ui::consts::SCR_W, crate::ui::consts::SCR_H);
    // the wedge peaks at its left margin and is gone by `HERO_SCRIM_W`; the right one peaks in the
    // bottom-right corner of the panel, which is the corner it is anchored to
    let none = theme::scrim(hero_scrim_a(HERO_SCRIM_W, strength));
    let edge = theme::scrim(hero_scrim_a(0.0, strength));
    let redge = theme::scrim(hero_scrim_right_a(sw, sh, strength));
    let mut q = [(Rect::new(0.0, 0.0, 0.0, 0.0), [none; 4]); 3];
    q[0] = (
        Rect::new(0.0, HERO_SCRIM_TOP, HERO_SCRIM_W, HERO_SCRIM_KNEE - HERO_SCRIM_TOP),
        [none, none, none, edge],
    );
    q[1] = (Rect::new(0.0, HERO_SCRIM_KNEE, HERO_SCRIM_W, sh - HERO_SCRIM_KNEE), [edge, none, none, edge]);
    if right {
        q[2] = (
            Rect::new(sw - HERO_SCRIM_R_W, HERO_SCRIM_R_TOP, HERO_SCRIM_R_W, sh - HERO_SCRIM_R_TOP),
            [none, none, redge, none],
        );
    }
    (q, if right { 3 } else { 2 })
}

/// Draw the hero corner scrim: the darkening for copy that sits directly on a full-bleed backdrop,
/// along the one axis a bottom-up ramp has nothing to say about.
///
/// Both heroes already paint a frame-wide atmospheric ramp, and both bottom-anchor their text
/// column well ABOVE the band where that ramp reaches strength — the HERO-72 title's cap top gets
/// ~0.11 of it. The ramp cannot simply be raised, because at any given y it is uniform across all
/// 1920px: enough alpha to rescue the title would put 60% black over the whole picture at the
/// title's y. So the corner the text is IN gets its own field instead, and the ramp goes on doing
/// the job it is good at (mood, and the depth under the shelf line).
///
/// `strength` is the screen's own hero fade — see [`hero_scrim_a`]. `right` adds the mirrored wedge
/// for a hero with a RIGHT-aligned column (detail's "Starring"); home has none and passes `false`.
///
/// Call it INSIDE the backdrop, before any hero content is drawn: it exists to darken artwork, and
/// a wedge over the text would be a dimmer, not a scrim.
pub(crate) fn hero_scrim(p: Painter, strength: f32, right: bool) {
    if strength <= 0.0 {
        return;
    }
    let (q, n) = hero_scrim_quads(strength, right);
    for (r, k) in q.iter().take(n) {
        p.grad4(*r, *k);
    }
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
        // the tab track's own material, faded in with the unfurl — one pair of weights for both
        p.rect_sheened(
            cap,
            cap.h * 0.5,
            theme::scrim_black(theme::TAB_TRACK_A_TOP * e),
            theme::scrim_black(theme::TAB_TRACK_A_BOT * e),
        );
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
    /// The **page** radius — a wait that owns a whole surface: the [`StatusOverlay`] read-out, and
    /// any screen whose content is simply not there yet. Sized to be read from the couch.
    ///
    /// Associated consts on the widget are the house pattern ([`PageDots::DOT`]/`PILL`/`GAP`): a
    /// spinner radius is neither a text size nor a gap, so it does not belong in `theme.rs`.
    pub const R_PAGE: f32 = 22.0;
    /// The **inline** radius — a mark that belongs to one line of text or one control, sized to sit
    /// on a `size::CAPTION` cap band. Anything that must sit *beside* something rather than *over*
    /// everything uses this.
    pub const R_INLINE: f32 = 12.0;
    /// Dot radius for a ring of radius `r` — the ONE place the ratio lives, so a layout can measure
    /// a spinner's real extent without re-deriving it (0.28·r ≈ the HUD spinner's original 3.4 at
    /// r=12, floored so a tiny ring still has visible dots).
    pub fn dot_r(r: f32) -> f32 {
        (r * 0.28).max(3.0)
    }
    pub fn new(cx: f32, cy: f32, r: f32) -> Self {
        // dot size scales WITH the ring radius, so a big spinner reads as a bigger spinner, not the
        // same tiny dots on a wider circle.
        Self { cx, cy, r, phase: 0, col: [1.0, 1.0, 1.0, 1.0], dots: 10, dot_r: Self::dot_r(r) }
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
        // A spinner is driven by a CLOCK, not a spring, so `ui::idle`'s spring instrumentation
        // cannot see it: before this line, a Home waiting on /hubs — the exact state the read-out
        // exists for — drew a STOPPED spinner. Reported here, in `draw`, and that is deliberate:
        // only a spinner actually ON SCREEN should hold the loop awake, whereas the six phase
        // accumulators that feed it tick unconditionally at the top of their screens' update.
        //
        // Reporting from a draw is sound in exactly one direction — it can only latch the gate ON
        // for the next frame, never off — and it self-sustains: the landing that started the load
        // presents frame 1, whose draw reports and so buys frame 2, until nothing draws a spinner.
        // It relies on `should_present` taking-and-clearing rather than `note_present` clearing
        // after the draw, which would destroy this report on the frame it is raised.
        crate::ui::idle::invalidate();
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

// ---- AmbientWash: the full-screen four-corner colour wash keyed to an item's artwork. ----

/// The **ambient wash** — a page-wide bilinear gradient taken from an item's PMS `UltraBlurColors`
/// corners, which is how home's backdrop, detail's below-hero ground and the person page all say
/// "this screen is about *this* artwork" without paying for a full-bleed image.
///
/// It exists as a component because the non-obvious part is not the gradient, it is the FADE.
/// [`Painter::ambient`](crate::ui::Painter::ambient) writes opaque pixels (its `dim` scales the
/// corners toward BLACK, which is a different thing), so a wash cannot be cross-faded from ONE
/// ITEM'S colours to ANOTHER'S by alpha at all — the only way is to spring each corner channel
/// toward its target and keep drawing at full strength. That is twelve springs, and a screen that
/// hand-rolls them ends up doing index arithmetic over a flat array. Fading a wash toward the app's
/// own GROUND is the one case the cascade *does* handle, and it belongs to the cascade: an alpha
/// below 1 mixes the corners toward [`theme::SURFACE_APP`] inside `Painter::ambient`, which is what
/// lets a whole page — wash included — dip for [`ui::nav`](crate::ui::nav)'s route transition.
///
/// Blend the corners toward a base surface with [`theme::mix`] before handing them over: a wash
/// keyed at full strength is a photograph, not a wash, and mixing toward [`theme::SURFACE_APP`]
/// means "no artwork" is simply the app's own flat ground with no special case.
#[derive(Clone, Copy)]
pub(crate) struct AmbientWash {
    /// corner-major, `Painter::ambient`'s order: top-left, top-right, bottom-right, bottom-left.
    corners: [[Spring; 3]; 4],
}

/// The luminance ceiling a GROUND colour is held to before it is mixed toward the surface.
/// UltraBlur corners are SAMPLED FROM THE ARTWORK, so a white poster hands us a near-white corner:
/// at any weight strong enough to see, that lifts the page off the palette's dark end and takes the
/// [`theme::TEXT_TERTIARY`] fine print sitting on it along — an uncapped white corner at
/// [`AmbientWash::GROUND_W`] puts `TEXT_TERTIARY`/`size::CAPTION` at **2.10:1**, under the 3:1
/// large-text floor; capped here the worst source (saturated green) is **3.67:1** and a white poster
/// **3.83:1**. A hard ceiling, not a tone map: only brightness is spent, the corner's hue and its
/// channel balance survive untouched, which is all a wash is saying. Rec.709 weights over the stored
/// display-encoded values — a ground-brightness knob, not colour management.
///
/// Why 0.42 and not higher: the legibility constraint only binds near **0.59** (green hits 3.0:1
/// there), so this is chosen design-first — a ceiling that let a white poster produce a mid-grey
/// page would betray the word "dark", and the contrast margin then falls out for free. Every number
/// above is MEASURED, by `a_ground_never_outshines_the_fine_print_that_sits_on_it`.
const GROUND_LUMA: f32 = 0.42;

/// One artwork corner, held under [`GROUND_LUMA`]. A scalar multiply (via [`theme::dim`]) rather
/// than a per-channel clamp, which would desaturate the corner instead of dimming it.
fn ground_capped(c: [f32; 3]) -> [f32; 4] {
    let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    theme::dim([c[0], c[1], c[2], 1.0], if y > GROUND_LUMA { GROUND_LUMA / y } else { 1.0 })
}

impl AmbientWash {
    /// The dissolve rate every page wash shares — deliberately slower than any focus spring (the
    /// person mock's `.5s ease`): the wash is atmosphere, and snapping it with the focus pop reads
    /// as the SCREEN changing rather than the subject. One rate, because two pages dissolving at two
    /// speeds are two different products.
    pub(crate) const K: f32 = 80.0;

    /// How far a GROUND leans from the app surface toward an item's own colour — the strength every
    /// item-keyed wash shares (the person page's focused-card state, the detail page's whole page).
    /// One value, because "this screen is about THIS artwork" should be the same statement wherever
    /// it is made; the per-corner SHAPE stays each screen's business (person tapers its bottom
    /// corners toward its shelves, detail keeps the artwork's own corner arrangement).
    ///
    /// The bound is what makes it safe to be the page's ground everywhere: with the source capped at
    /// [`GROUND_LUMA`], the brightest ground is ≈60/255 and the darkest (black corners) ≈33/255 —
    /// clear of the near-black 25/255 that [`theme::SURFACE_APP`]'s own doc rejects as too dark for
    /// a card shadow to read against. No floor constant is needed; `GROUND_W ≤ 0.26` IS the floor.
    pub(crate) const GROUND_W: f32 = 0.26;

    /// How close a wash must be to the colour it is drawn over before a screen stops drawing it —
    /// [`is_flat`](Self::is_flat)'s epsilon, ONE 8-bit code, below which the panel cannot show a
    /// difference. ([`theme::SURFACE_APP`] is snapped to exact 8-bit codes for the `GL_DITHER`
    /// reason in its own doc, so that is the unit this is reasoned in.)
    ///
    /// It lives beside the type rather than in a screen because the skip is worth ~2.07M fragments
    /// a frame on two different pages, and it was a private `home.rs` constant while `detail.rs`
    /// simply lacked the test — one value here is what stops the two from drifting apart again.
    pub(crate) const FLAT_EPS: f32 = 1.0 / 255.0;

    /// A dissolve target: corner `i` mixed from [`theme::SURFACE_APP`] toward `src[i]` by `w[i]`
    /// ("how much of this source shows through at that corner"). The mix is the TYPE's contract (see
    /// the docs above) rather than a loop each screen writes, so "no artwork" is the app's own flat
    /// ground on every page **by construction** — `w = 0` returns exactly the surface. Colours that
    /// came from ARTWORK go through [`keyed`](Self::keyed), which caps them first.
    pub(crate) fn target(src: [[f32; 4]; 4], w: [f32; 4]) -> [[f32; 4]; 4] {
        std::array::from_fn(|i| theme::mix(theme::SURFACE_APP, src[i], w[i]))
    }

    /// [`target`](Self::target) for corners taken from an item's `UltraBlurColors` envelope
    /// (`PmsMovie::blur` / `metadata::Detail::blur`), each held under [`GROUND_LUMA`] first. Every
    /// artwork-keyed wash goes through here; a palette token (the resting warm tint) does not need
    /// it, because we chose that value.
    pub(crate) fn keyed(blur: [[f32; 3]; 4], w: [f32; 4]) -> [[f32; 4]; 4] {
        Self::target(blur.map(ground_capped), w)
    }

    /// A wash resting flat on one colour — what a page opens as before any item keys it.
    pub(crate) fn flat(c: [f32; 4]) -> Self {
        AmbientWash { corners: [[Spring::at(c[0]), Spring::at(c[1]), Spring::at(c[2])]; 4] }
    }
    /// Jump straight to `target` (no dissolve) — on mount, so the PREVIOUS item's colours never
    /// dissolve across a page that has just changed subject.
    pub(crate) fn jump(&mut self, target: [[f32; 4]; 4]) {
        for (c, t) in self.corners.iter_mut().zip(target) {
            for (sp, v) in c.iter_mut().zip(t) {
                sp.jump(v);
            }
        }
    }
    /// Dissolve toward `target` at rate `k`. Every channel shares one rate, so the corners move as
    /// one wash rather than twelve independent fades.
    pub(crate) fn step(&mut self, target: [[f32; 4]; 4], k: f32, dt: f32) {
        for (c, t) in self.corners.iter_mut().zip(target) {
            for (sp, v) in c.iter_mut().zip(t) {
                sp.step(v, k, dt);
            }
        }
    }
    /// Is this wash within `eps` of the flat colour `c` on every corner channel? A wash that has
    /// resolved to the app's own clear colour is a ~2M-fragment full-screen fill that changes
    /// nothing, and a screen must be able to skip it. (`eps` is naturally one 8-bit code —
    /// [`theme::SURFACE_APP`] is snapped to exact codes for the `GL_DITHER` reason in its own doc,
    /// so that is the unit this value is reasoned in.) Its own method because the corner springs are
    /// private.
    pub(crate) fn is_flat(&self, c: [f32; 4], eps: f32) -> bool {
        self.corners.iter().all(|q| q.iter().zip(c).all(|(s, v)| (s.pos - v).abs() <= eps))
    }
    /// Paint it over `r`. Opaque — this REPLACES what is under it (see the type docs), so it belongs
    /// at the bottom of a screen's draw, standing in for the flat clear.
    pub(crate) fn draw(&self, p: Painter, r: Rect) {
        p.ambient(r, 1.0, self.corners.map(|c| [c[0].pos, c[1].pos, c[2].pos]));
    }
}

// ---- StatusOverlay: a centred "something is happening / something failed" read-out — a Spinner
// (or nothing, for a terminal state) above one line of copy. The player HUD renders it for the
// states where NO PICTURE IS ON THE PANEL (Resolving/Connecting/Buffering/Seeking before this
// session's first frame, and Error, which is what a black screen used to be) — a seek over a live
// picture belongs to the transport's inline spinner instead, and `player_hud::busy_surface` is the
// ONE place that division is written down. `kind` picks the treatment, not the words: the caller
// supplies the caption so the state machine stays the single source of that string.
//
// The `frame` is THE AREA THE WAIT IS ABOUT, and the read-out centres on it — pass the region whose
// content is missing, not a region carved out to dodge other chrome. Home's catalog is the whole
// screen (`Rect::FULL`); the person page's shelves are one band, so it passes the band; the player's
// picture is the whole panel, so it passes `Rect::FULL` too. Carving the frame down to "avoid"
// nearby chrome pushes the read-out OFF the optical centre, which is exactly what the player's
// deleted `OVERLAY_BOTTOM` did — it centred the block at y=370 on a 1080 panel. ----
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
    /// How far the read-out's ink reaches ABOVE the frame centre: the spinner ring plus its dots
    /// plus the `space::XS` that separates it from the caption. The caption half is deliberately NOT
    /// here — it needs `text::text_height`, which the host suite cannot link — and it is the SMALLER
    /// half, so this doubles as the conservative bound a layout test can assert against.
    pub fn above() -> f32 {
        Spinner::R_PAGE + theme::space::XS + Spinner::R_PAGE + Spinner::dot_r(Spinner::R_PAGE)
    }
}
impl View for StatusOverlay {
    fn draw(&self, e: &Env, p: Painter) {
        // spinner above, caption below, the pair centred on the frame
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
            Spinner::new(self.frame.cx(), cy - Spinner::R_PAGE - theme::space::XS, Spinner::R_PAGE)
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
// (0 = subtitles/CC, 1 = audio, 2 = more/overflow). Focused = accent fill + dark icon; idle = faint
// fill + white icon. Mirrors the mockup's round icon buttons. ----
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
            2 => Icon::More,
            _ => Icon::Cc,
        };
        let s = (r.w * DISC_ICON_RATIO).round();
        let ir = Rect::new(r.x + (r.w - s) * 0.5, r.y + (r.h - s) * 0.5, s, s);
        crate::ui::icons::draw(p, id, ir, ink);
    }
}

// ---- FieldList: a NON-INTERACTIVE key/value read-out ---------------------------------------
//
// The diagnostics overlay's list primitive (`ui/stats.rs`), and the reason it is not a
// `TableView`: that is a SELECTION widget. It paints an accent pill under row `sel` on every draw
// with no "nothing selected" mode, its rows are 60px so ~25 of them measure 1540 against a 1080
// panel and SCROLL behind a scissor, and a row is `label` + optional sub-line + badges — there is
// no right-hand value column at all. A read-out needs the opposite of all three: no selection, no
// scrolling (a panel the user must scroll is two photographs and a chance of missing the line that
// mattered), and a fixed value column. Nothing here was close, which is the condition ui/CLAUDE.md
// sets for a new component.
//
// It owns no state, no focus and no springs: hand it a slice and a frame and it draws.

/// A read-out value's severity. Carried by a WORD in the value text as well as by this tint —
/// a phone photograph of a television chroma-subsamples, so hue alone must never be the signal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Normal,
    /// something is wrong here, and this is the row to read first
    Fault,
}

/// One line of a [`FieldList`]. `val: None` makes it a SECTION heading rather than a pair.
pub struct Field {
    pub key: &'static str,
    pub val: Option<String>,
    pub tone: Tone,
}

impl Field {
    pub fn new(key: &'static str, val: impl Into<String>) -> Self {
        Self { key, val: Some(val.into()), tone: Tone::Normal }
    }
    /// mark the row as the fault — see [`Tone`]
    pub fn fault(mut self, bad: bool) -> Self {
        if bad {
            self.tone = Tone::Fault;
        }
        self
    }
    /// a group heading, drawn as a quiet caption above its rows
    pub fn section(key: &'static str) -> Self {
        Self { key, val: None, tone: Tone::Normal }
    }
}

/// Row pitch. Values are `size::BODY` (28); this is that plus air, and it is what bounds how many
/// fields the overlay may carry — see `stats::COLUMN_ROWS`.
pub const FIELD_ROW_H: f32 = 36.0;
/// Width of the key column inside a [`FieldList`] frame. Keys are right-aligned against it and
/// values start one `space::MD` later, so every value in a column shares an x — which, with the
/// font's tabular digits (all ten share one advance), is what makes the numbers line up.
pub const FIELD_KEY_W: f32 = 178.0;
/// Width a [`FieldList`] column needs: the key gutter plus room for the longest value.
pub const FIELD_COL_W: f32 = FIELD_KEY_W + theme::space::MD + 292.0;

pub struct FieldList<'a> {
    pub fields: &'a [Field],
    pub frame: Rect,
}

impl<'a> FieldList<'a> {
    pub fn new(fields: &'a [Field], frame: Rect) -> Self {
        Self { fields, frame }
    }
    /// How tall this list draws — so a caller can size or split its columns without re-deriving
    /// the pitch.
    pub fn height(n: usize) -> f32 {
        n as f32 * FIELD_ROW_H
    }
}

impl View for FieldList<'_> {
    fn draw(&self, _e: &Env, p: Painter) {
        let vx = self.frame.x + FIELD_KEY_W + theme::space::MD;
        let vw = (self.frame.w - FIELD_KEY_W - theme::space::MD).max(0.0);
        for (i, f) in self.fields.iter().enumerate() {
            let y = self.frame.y + i as f32 * FIELD_ROW_H;
            match &f.val {
                // a section heading spans the whole width and carries no value
                None => {
                    // A heading differs from a key by POSITION (full width, left-aligned, where a
                    // key is right-aligned into its gutter) and by weight — not by size. Dropping
                    // it to `MICRO` to save a few pixels is the one thing that token's doc forbids.
                    if let Ok(cs) = CString::new(f.key) {
                        Label::new(cs.as_ptr(), theme::size::CAPTION, theme::TEXT_SECONDARY)
                            .bold()
                            .draw(p, Rect::new(self.frame.x, y, self.frame.w, FIELD_ROW_H));
                    }
                }
                Some(v) => {
                    if let Ok(cs) = CString::new(f.key) {
                        Label::new(cs.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
                            .h(HAlign::Right)
                            .draw(p, Rect::new(self.frame.x, y, FIELD_KEY_W, FIELD_ROW_H));
                    }
                    let ink = if f.tone == Tone::Fault { theme::DANGER } else { theme::TEXT_PRIMARY };
                    // ELIDE. Every other bounded-width text site in `ui/` does, and this one has a
                    // value it cannot bound: the exported windowId is a compositor-assigned
                    // char[64], which at this size is ~3x the value column and would run off the
                    // card — on the one firmware family that cannot be tested here. `elide` is
                    // memoised by (string, budget, size, bold) and the panel re-formats at 2 Hz.
                    let bold = i32::from(f.tone == Tone::Fault);
                    let v = crate::text::elide(v, vw, theme::size::BODY, bold, false);
                    if let Ok(cs) = CString::new(v.as_str()) {
                        let mut l = Label::new(cs.as_ptr(), theme::size::BODY, ink);
                        if f.tone == Tone::Fault {
                            l = l.bold();
                        }
                        l.draw(p, Rect::new(vx, y, vw, FIELD_ROW_H));
                    }
                }
            }
        }
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
    /// see [`TabPill::mix`] — `None` = this pill owns its own state fill (the boolean model).
    mix: Option<(f32, f32)>,
}
impl TabPill {
    /// pill width for a `chars`-long label at `sz` (label advance + horizontal padding). Covers
    /// the LABEL only — a pill that also carries a [`TabNote`] must add [`note_w`](Self::note_w),
    /// or the note is drawn outside the fill.
    pub fn width(chars: usize, sz: c_int) -> f32 {
        chars as f32 * sz as f32 * 0.56 + 44.0
    }
    pub fn new(label: *const c_char, sz: c_int, frame: Rect) -> Self {
        Self {
            frame,
            label,
            sz,
            focused: false,
            style: TabStyle::Button,
            note: TabNote::None,
            plated: false,
            mix: None,
        }
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
    /// The BOOLEAN segmented model. It has **no caller today**: both of the app's segmented rows —
    /// the shared top tab bar and the detail season tabs — are [`TabStrip`]s now, and a strip inks its
    /// pills through [`mix`](Self::mix) so the highlight can travel between them. It is kept, with its
    /// four arms below, because it is the right answer for a segmented control that is NOT in a strip
    /// (nothing there to travel), and because deleting it would take [`TabStyle::Segment`] with it and
    /// leave a one-variant enum. Do not reach for it inside a strip: two pills would each paint their
    /// own fill and the capsules would have nothing left to do.
    pub fn segment(mut self, selected: bool) -> Self {
        self.style = TabStyle::Segment { selected };
        self
    }
    /// Strip-driven ink: `focus`/`selected` as 0..1 **mixes** rather than booleans, because the fills
    /// they used to imply are now travelling capsules the STRIP draws ([`TabStrip`]). A mixed pill
    /// paints no state fill of its own — only a plated strip's idle ground, which is a constant per
    /// pill and not a state (and which retires as the opaque focus capsule arrives under it, so the
    /// composite at full focus is exactly the `ACCENT` it always was).
    ///
    /// Leave it unset for a STANDALONE pill — [`TabStyle::Button`], the player HUD's Info/Chapters —
    /// which owns its fill and has no strip to travel in; those keep the boolean model verbatim.
    pub fn mix(mut self, focus: f32, selected: f32) -> Self {
        self.mix = Some((focus, selected));
        self
    }
    /// The ink a strip-driven pill wears at mixes `(focus, selected)` — `TEXT_TERTIARY` →
    /// `TEXT_PRIMARY` as the selection capsule arrives, then all the way to `ACCENT_INK` as the focus
    /// capsule covers it. Nested in that order because focus OUTRANKS selection, which is the order
    /// the boolean arms below are written in.
    ///
    /// Pure and split out of the draw so the states no STILL capture can catch — the mid-travel ones,
    /// where a partly covered pill is inking its whole label — are an executable contract rather than
    /// an eyeball. It is linear in coverage on purpose: [`cap_cover`] is the one rule both the fill
    /// and the ink read, so they cannot disagree about where the capsule is. If a device capture ever
    /// says the labels "dim when I move", shaping this one call (`focus.powi(2)`, a smoothstep) is the
    /// whole fix — but do it HERE, not with a second spring, or the ink starts leading the fill.
    pub(crate) fn mixed_ink(focus: f32, selected: f32) -> [f32; 4] {
        theme::mix(
            theme::mix(theme::TEXT_TERTIARY, theme::TEXT_PRIMARY, selected.clamp(0.0, 1.0)),
            crate::ui::ACCENT_INK,
            focus.clamp(0.0, 1.0),
        )
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
        // (fill, ink, weight 0..1). Two state models, and the strip-driven one lerps between exactly
        // the same three ink roles the boolean match picks from, so a mixed pill at 0/1 is the old
        // look to the bit (see `the_ink_a_pill_wears_is_exactly_the_capsule_over_it`).
        //
        // Boolean model: a highlighted (focused) tab is a bright ACCENT pill; a selected-but-
        // unfocused segment is a subtle pill; a plain segment is dim text with no pill at all.
        // Legibility over art is the CONTAINER's job (the tab-bar track in `draw_tab_row`), not
        // the segment's — the detail season tabs use this element bare on a controlled ground.
        let (fill, ink, bold_mix) = match self.mix {
            Some((fm, sm)) => {
                let (fm, sm) = (fm.clamp(0.0, 1.0), sm.clamp(0.0, 1.0));
                let ink = Self::mixed_ink(fm, sm);
                // The plated strip's idle ground stays the PILL's, because it is a constant per pill
                // rather than a state — but it must not lie on top of the OPAQUE focus capsule the
                // strip drew under it (0.08 of white over `ACCENT` is a ~2/255 lift and a bright rim
                // exactly where the capsule is brightest), so it retires as that capsule arrives. The
                // SELECTION capsule needs no such treatment: two whites composite to `a + b − ab`
                // whichever way round they are stacked, which is how `TAB_PLATE_SELECTED_OVER` lands
                // back on `TAB_PLATE_SELECTED`'s .20 from underneath.
                let plate_a = theme::TAB_PLATE_IDLE[3] * (1.0 - fm);
                let fill = (self.plated && plate_a > 0.002).then(|| theme::with_a(theme::TAB_PLATE_IDLE, plate_a));
                (fill, ink, sm.max(fm))
            }
            None => match self.style {
                TabStyle::Button if self.focused => (Some(crate::ui::ACCENT), crate::ui::ACCENT_INK, 1.0),
                TabStyle::Button => (Some(theme::CONTROL_IDLE_FILL), theme::CONTROL_IDLE_INK, 1.0),
                TabStyle::Segment { .. } if self.focused => (Some(crate::ui::ACCENT), crate::ui::ACCENT_INK, 1.0),
                TabStyle::Segment { selected: true } if self.plated => (Some(theme::TAB_PLATE_SELECTED), theme::TEXT_PRIMARY, 1.0),
                TabStyle::Segment { .. } if self.plated => (Some(theme::TAB_PLATE_IDLE), theme::TEXT_TERTIARY, 0.0),
                TabStyle::Segment { selected: true } => (Some(theme::OVERLAY_FOCUS_PILL), theme::TEXT_PRIMARY, 1.0),
                TabStyle::Segment { .. } => (None, theme::TEXT_TERTIARY, 0.0),
            },
        };
        if let Some(bg) = fill {
            p.rrect(r, r.h * 0.5, r.h * 0.5, bg);
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
        // on an unbolded tab is the same slack the label alone already had. The one exception is the
        // crossfade below, which has two painted widths and so no single answer — it falls back to
        // the caller's own bold advance rather than letting the tick slide as the weights swap.
        let nw = Self::note_w(self.note);
        let frame = Rect::new(r.x, r.y, r.w - nw, r.h);
        let run = |p: Painter, bold: bool| {
            let mut lab = Label::new(self.label, self.sz, ink).h(HAlign::Center);
            if bold {
                lab = lab.bold();
            }
            lab.draw(p, frame)
        };
        // WEIGHT crossfades; it does not tween. A bold run and a regular run are two rasterizations
        // — the same reason `theme.rs`'s size ladder says a SIZE is crossfaded rather than animated,
        // and the way `person.rs` spells its band condense. Two draws happen only while a capsule is
        // genuinely mid-travel over THIS pill: at most two pills, for ~200 ms. Both boolean states
        // land squarely in the single-draw branches, so nothing that exists today pays for this.
        let lw = if bold_mix > 0.98 {
            run(p, true)
        } else if bold_mix < 0.02 {
            run(p, false)
        } else {
            run(p.alpha(1.0 - bold_mix), false);
            run(p.alpha(bold_mix), true);
            crate::text::text_width(self.label, self.sz, 1)
        };
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

// ---- Tab strip MOTION: the travelling capsules. ----
// A `TabPill` is a retui LEAF: it has no idea its neighbours exist, so it structurally cannot
// animate *between* pills — which is why the selection used to vanish from one pill and reappear on
// the next in a single frame. The highlight is therefore hoisted out of the pill and into the STRIP,
// which knows every pill's span and can carry one fill from one to another. The pill keeps only its
// label, its constant ground, and the ink it derives from how covered it is.
//
// Shared by the top tab bar (`draw_tab_row`) and the detail page's season tabs, per the owner
// directive recorded above [`TAB_PILL_H`]: the two rows are ONE control, and one control has one
// motion. The stiffnesses below are matched to springs that already exist rather than invented.

/// Stiffness of a tab strip's travelling capsules. Deliberately IDENTICAL to [`K_TAB_SCROLL`]: the
/// capsule rides *inside* the strip it marks, and a highlight that settles at a different rate from
/// the row under it reads as two objects instead of one control. (The detail season row springs its
/// own scroll at the same 240 — the directive above [`TAB_PILL_H`] covers their motion too.)
const K_TAB_CAP: f32 = K_TAB_SCROLL;
/// Stiffness of a capsule's ALPHA — the app's shared appear spring
/// ([`crate::ui::popover::K_APPEAR`]), because entering or leaving a row is a FADE, not a journey.
const K_TAB_CAP_A: f32 = crate::ui::popover::K_APPEAR;
/// Below this alpha a capsule is not on screen, so it LANDS on its next pill instead of gliding to
/// it (see [`Capsule::step`]). 0.02 is picked to be *invisibly* small rather than merely small: the
/// faintest capsule is [`theme::OVERLAY_FOCUS_PILL`] at .14, so at this alpha it composites to
/// .0028 of white — under one display code on the panel's 8-bit framebuffer — and a jump there
/// cannot be seen however far it travels.
const CAP_LAND_A: f32 = 0.02;

/// How much of pill `pill` (content-space `(x, w)`) a capsule spanning `cap` covers, 0..1 — a 1-D
/// [`Rect::intersect`]. This is the ONE rule a pill's ink is derived from, so the ink can never
/// disagree with the fill sliding under it: a dark label left behind on a pill the capsule has
/// already left is unreadable, and that is exactly what a separate per-pill ink spring would
/// produce. A zero-width pill covers nothing (and, more to the point, does not divide by zero).
fn cap_cover(pill: (f32, f32), cap: (f32, f32)) -> f32 {
    if pill.1 <= 0.0 {
        return 0.0;
    }
    let lo = pill.0.max(cap.0);
    let hi = (pill.0 + pill.1).min(cap.0 + cap.1);
    ((hi - lo) / pill.1).clamp(0.0, 1.0)
}

/// A tab highlight that TRAVELS between pills instead of being repainted onto a new one. Two springs
/// for its content-space geometry — left edge AND width, so a wide pill *morphs* into a narrow one
/// rather than teleporting — plus one for alpha, which is how it enters and leaves a row without
/// streaking across the strip from wherever it was last parked.
#[derive(Clone, Copy)]
pub(crate) struct Capsule {
    x: Spring,
    w: Spring,
    a: Spring,
    /// The pill index this capsule is bound to, or -1 for "nothing to mark". Tracked so a capsule
    /// that has never been placed can tell that apart from one resting at content x = 0 (which is a
    /// real position: it is pill 0).
    at: i32,
}

impl Capsule {
    pub(crate) const fn new() -> Self {
        Capsule { x: Spring::at(0.0), w: Spring::at(0.0), a: Spring::at(0.0), at: -1 }
    }
    /// One frame toward pill `i`'s content-space `(x, w)`. `None` = nothing to mark (focus left the
    /// row, or the index went stale after a section refetch): HOLD the position and fade out, so
    /// focus leaving and returning to the SAME pill is a fade, not a round trip.
    ///
    /// A capsule that is not on screen ([`CAP_LAND_A`]) LANDS rather than glides. Without this, the
    /// first frame of the Library screen — and every return of focus to the row — would start with a
    /// bright capsule flying in from the pill the user was on two screens ago.
    fn step(&mut self, target: Option<(usize, (f32, f32))>, dt: f32) {
        match target {
            Some((i, (x, w))) => {
                if self.at < 0 || self.a.pos < CAP_LAND_A {
                    self.x.jump(x);
                    self.w.jump(w);
                }
                self.at = i as i32;
                self.x.step(x, K_TAB_CAP, dt);
                self.w.step(w, K_TAB_CAP, dt);
                self.a.step(1.0, K_TAB_CAP_A, dt);
            }
            None => {
                self.at = -1;
                self.a.step(0.0, K_TAB_CAP_A, dt);
            }
        }
    }
    /// Content-space `(x, w)` as drawn this frame.
    #[inline]
    fn span(&self) -> (f32, f32) {
        (self.x.pos, self.w.pos)
    }
    #[inline]
    fn alpha(&self) -> f32 {
        self.a.pos.clamp(0.0, 1.0)
    }
    /// How strongly pill `pill` is wearing this capsule right now — coverage × alpha.
    #[inline]
    fn mix(&self, pill: (f32, f32)) -> f32 {
        self.alpha() * cap_cover(pill, self.span())
    }
}

/// The motion state of ONE tab strip: the subtle capsule marking the SELECTED tab and the bright one
/// marking the FOCUSED tab, both travelling. Two, not one, because they are independently placed —
/// on the Library screen the selected tab is the section you are browsing while focus walks the row
/// — and one capsule cannot be in two places. Held as a value, not a global: the top row keeps one
/// static instance, the detail page keeps one per `DetailView` (so a new item's strip starts clean).
#[derive(Clone, Copy)]
pub(crate) struct TabStrip {
    sel: Capsule,
    foc: Capsule,
}

impl TabStrip {
    pub(crate) const fn new() -> Self {
        TabStrip { sel: Capsule::new(), foc: Capsule::new() }
    }
    /// Step both capsules. `span(i)` resolves pill `i`'s content-space `(x, w)` and MUST be the same
    /// function the caller lays its pills out with — that is what keeps a capsule from ever landing
    /// off a pill. A negative or out-of-range index resolves to `None` (nothing to mark).
    pub(crate) fn update(
        &mut self,
        selected: c_int,
        focused: c_int,
        span: impl Fn(usize) -> Option<(f32, f32)>,
        dt: f32,
    ) {
        let pick = |i: c_int| -> Option<(usize, (f32, f32))> {
            let i = usize::try_from(i).ok()?;
            span(i).map(|s| (i, s))
        };
        let (sel_t, foc_t) = (pick(selected), pick(focused));
        // The focus capsule's REAL spring target, read before the step. Every other probe site
        // passes one, and it is the whole of what the diagnostic measures: with `pos` handed in as
        // its own target the overshoot and settle-frame numbers degenerate to nothing on every
        // frame, including the travelling ones the probe exists to look at. `None` means "hold
        // where you are" (see [`Capsule::step`]), so on those frames the target IS the position.
        let foc_x = foc_t.map(|(_, (x, _))| x).unwrap_or(self.foc.x.pos);
        self.sel.step(sel_t, dt);
        self.foc.step(foc_t, dt);
        crate::ui::anim::probe("tabstrip.foc", self.foc.x.pos, self.foc.x.vel, foc_x, dt);
    }
    /// Draw both capsules in the painter's own (already scroll-translated) CONTENT space, for a strip
    /// whose pills stand `h` tall at `top`. Selection first, focus over it — when they are on the same
    /// pill the bright one wins, exactly as the boolean match ordered its arms. `plated` = the strip
    /// lays its own per-pill ground (the detail season tabs), so the selection capsule takes the value
    /// that composites OVER that ground instead of replacing it.
    ///
    /// Call this BEFORE the pill loop: the capsules are the pills' ground, and a label drawn under an
    /// opaque focus capsule is a label nobody can read.
    pub(crate) fn draw(&self, p: Painter, top: f32, h: f32, plated: bool) {
        let cap = |c: &Capsule, col: [f32; 4]| {
            let (x, w) = c.span();
            let a = c.alpha();
            // sub-code alpha or a sub-pixel width is nothing on screen but still a full rrect pass
            if a > 0.004 && w > 0.5 {
                p.alpha(a).rrect(Rect::new(x, top, w, h), h * 0.5, h * 0.5, col);
            }
        };
        cap(&self.sel, if plated { theme::TAB_PLATE_SELECTED_OVER } else { theme::OVERLAY_FOCUS_PILL });
        cap(&self.foc, crate::ui::ACCENT);
    }
    /// The `(focus, selected)` mixes pill `pill` (content-space `(x, w)`) should ink itself with —
    /// hand straight to [`TabPill::mix`].
    pub(crate) fn mixes(&self, pill: (f32, f32)) -> (f32, f32) {
        (self.foc.mix(pill), self.sel.mix(pill))
    }
}

// ---- The shared top tab row: profile chip leads at the margin, the pills (Home | <library
// sections>) sit CENTERED — the tvOS tab-bar idiom. Drawn by BOTH the Home screen and the
// Library screen so they read as one global tab bar; the pill rects live here so both
// screens' pointer paths share [`tab_pill_at`]. ----
/// Home + every library section **that gets a pill**. Focus walking clamps to this — the invariant
/// is still "a pill that can't be drawn must never be focusable", but it now holds by construction
/// rather than by a cap: the strip scrolls horizontally inside its track (see
/// [`tab_scroll_target`]), so every pill can be brought into view and every pill is focusable.
/// This used to be a hard `MAX_TABS = 5`, which left a fifth `movie`/`show` section unreachable
/// from the UI even though `browse` discovers it and the grid browses it fine.
///
/// It is `browse::tab_count`, NOT `section_count`, and the difference is a design call rather than
/// a detail: **a pill is a TYPE, never a person**. The strip names your own libraries (plus any
/// type only a friend has), source lives in the Library toolbar's Source chip one line below, and
/// so the strip is the same width at one friend or at ten. With a single source the two counts are
/// identical, which is why nothing about a one-server install changed.
pub(crate) fn tab_count() -> usize {
    1 + crate::browse::tab_count()
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
/// The y below which a screen's content is clear of the shared top chrome — the tab track's bottom
/// edge ([`TOP_BAR_Y`] + the pill height + the track's inset). Exposed so a screen that lets art
/// overflow UPWARD out of its layout band ([`crate::ui::hero_logo`]) can ASSERT its clearance in a
/// host test instead of leaving it to a device capture.
pub(crate) const TOP_BAR_BOTTOM: f32 = TOP_BAR_Y + TAB_PILL_H + TAB_TRACK_PAD; // 112
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
/// The top row's travelling capsules ([`TabStrip`]). A static for the same reason [`TAB_SCROLL`] is
/// one: ONE tab bar is drawn by two screens, and the capsule must carry ACROSS the Home→Library
/// route flip — that carry IS the transition. Stepped from [`tab_row_update`], so it moves on the
/// PRESS frame: Library hands its *pending* section down here (`library::view_section`), which is
/// what puts the capsule on the new pill while the grid is still dissolving under it.
static mut TOP_STRIP: TabStrip = TabStrip::new();
// label + width cache keyed on browse::tabs_gen(): rebuilding the CStrings and
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
/// `browse::tabs_gen()` moves. Shared by the per-frame scroll step and the draw, so the two
/// can't measure the strip differently.
///
/// The key is the STRIP's generation, not the section table's, and with several sources that is no
/// longer the same number: the table's generation moves for every source whose libraries land and
/// every count that arrives, while the row only changes when the projection does. Most of those
/// landings fold onto pills already drawn (a friend's films ride your *Movies* pill), so keying on
/// the table re-measured every pill in the row, several times a boot, on Home's hot path — for a
/// strip that had not moved.
///
/// Deliberately a closure and not a `&'static` getter: the borrow points into `TAB_CACHE`, and a
/// nested rebuild would free the `CString`s a caller is still handing to `TabPill` as a raw
/// `*const c_char`. Scoping it here makes that impossible to write by accident — so do NOT call
/// this again from inside `f`.
fn with_tab_metrics<R>(f: impl FnOnce(&[std::ffi::CString], &[f32]) -> R) -> R {
    use std::ffi::CString;
    use std::ptr::addr_of_mut;
    let gen = crate::browse::tabs_gen();
    let cache = unsafe { &mut *addr_of_mut!(TAB_CACHE) };
    if cache.as_ref().map(|c| c.0 != gen).unwrap_or(true) {
        let nsec = crate::browse::tab_count();
        let mut labels: Vec<CString> = Vec::with_capacity(1 + nsec);
        labels.push(CString::new("Home").unwrap_or_default());
        for i in 0..nsec {
            labels.push(CString::new(crate::browse::tab_title(i)).unwrap_or_default());
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
///
/// It also steps the row's travelling capsules ([`TOP_STRIP`]), off the very same `selected`/
/// `focused` the scroll reads — so every caller of the shared row gets the motion by construction
/// rather than by remembering to call a second thing.
pub(crate) fn tab_row_update(selected: c_int, focused: c_int, dt: f32) {
    use std::ptr::{addr_of, addr_of_mut};
    // The bar is CONTINUOUS chrome across the Home↔Library route change and the capsule has to start
    // travelling on the PRESS frame, before the route flips — so the selection is the NAV's pending
    // one whenever there is one, exactly as `library::view_section` is the pending one for that
    // screen's own chips. Resolved HERE, in the one function both screens call, so neither can
    // forget it and the two can never disagree about which pill is lit. (It also carries into `idx`
    // below, so a strip that must SCROLL to reach the destination starts scrolling on the press
    // frame too.)
    let selected = crate::ui::nav::view_tab(selected);
    let idx = if focused >= 0 { focused } else { selected.max(0) } as usize;
    let cur = unsafe { addr_of!(TAB_SCROLL).read() };
    // Both reads happen inside the ONE `with_tab_metrics` closure — its doc forbids nesting, and the
    // capsules must be placed from the SAME widths the pills are laid out with, so a capsule can
    // never come to rest somewhere no pill is.
    let t = with_tab_metrics(|_, w| {
        let target = tab_scroll_target(w, idx, cur.pos);
        let span = |i: usize| (i < w.len()).then(|| (tab_pill_x(w, i), w[i]));
        unsafe { (*addr_of_mut!(TOP_STRIP)).update(selected, focused, span, dt) };
        target
    });
    unsafe { (*addr_of_mut!(TAB_SCROLL)).step(t, K_TAB_SCROLL, dt) };
    let s = unsafe { addr_of!(TAB_SCROLL).read() };
    crate::ui::anim::probe("tabrow.scroll", s.pos, s.vel, t, dt);
}

/// Put pill `idx` on screen at once (no glide). For screen ENTRY — the Library screen opened
/// straight into a far-right section (the `/tmp/plxnative-library=N` boot, or a section restored
/// from the saved view) must show its own tab, and a long slide on arrival would be motion the
/// user never asked for. Mirrors `detail.rs`'s `tab_hscroll.jump` on a fresh detail page.
///
/// It deliberately does NOT place the capsules. Their own landing rule ([`Capsule::step`]) already
/// jumps an *unplaced* one, which covers the boot-straight-into-a-section case; leaving the placed
/// case to glide is exactly what makes OK on Home's `Movies` pill read as the selection travelling
/// there rather than blinking there.
pub(crate) fn tab_row_reveal(idx: usize) {
    use std::ptr::{addr_of, addr_of_mut};
    let cur = unsafe { addr_of!(TAB_SCROLL).read() };
    let t = with_tab_metrics(|_, w| tab_scroll_target(w, idx, cur.pos));
    unsafe { (*addr_of_mut!(TAB_SCROLL)).jump(t) };
}

/// Draw the centered pill row. Records the rects for [`tab_pill_at`].
///
/// It takes no `selected`/`focused`: both states are now the strip's travelling capsules, placed once
/// per frame by [`tab_row_update`] from exactly those two values. Passing them here as well would let
/// a screen's draw and its update disagree about which tab is lit — which is precisely the class of
/// bug a single source of the row's state removes.
pub(crate) fn draw_tab_row(p: Painter) {
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
        // dark-material weight (`theme::TAB_TRACK_TOP` holds the reasoning): light enough to keep a
        // hint of the art, dark enough that the TEXT_TERTIARY plain segments hold contrast even over
        // near-white art
        p.rect_sheened(track, track.h * 0.5, theme::TAB_TRACK_TOP, theme::TAB_TRACK_BOT);
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
        // The selection/focus fills, as ONE travelling capsule each ([`TOP_STRIP`]) rather than a
        // boolean fill per pill. They were placed in content space with `tab_pill_x`, so a single
        // translate puts them and the pills on the same ruler; drawn first, because they are the
        // pills' ground. This strip is NOT plated — it sits inside the tab-bar track above, which
        // already is the ground, so the pills paint no fill of their own at all here.
        let cp = p.translate(x0 - sx, 0.0);
        unsafe { &*addr_of!(TOP_STRIP) }.draw(cp, TOP_BAR_Y, TAB_PILL_H, false);
        rects.reserve(n);
        for i in 0..n {
            // ONE prefix sum per pill: `tab_pill_x` is O(i), and it was walked twice here — once for
            // the rect and again for the capsule coverage — so the row re-summed its own width every
            // frame for nothing.
            let px = tab_pill_x(widths, i);
            let r = Rect::new(x0 + px - sx, TOP_BAR_Y, widths[i], TAB_PILL_H);
            // ONE rule for "is this pill on screen": its rect clipped to the viewport. The clipped
            // rect is what gets recorded for the hit test AND what decides whether to draw at all
            // (a 12-library server would otherwise lay out three rows' worth of text the scissor
            // throws away), so the two can never disagree about a sliver at the edge.
            let vis = r.intersect(view);
            rects.push(vis);
            if vis.w > 0.5 {
                // ink comes from how covered this pill is by the capsules above — never from its own
                // booleans, or a mid-travel label could darken toward ACCENT_INK with nothing bright
                // under it (`cap_cover` is the one rule both sides read).
                let (fm, sm) = unsafe { &*addr_of!(TOP_STRIP) }.mixes((px, widths[i]));
                TabPill::new(labels[i].as_ptr(), theme::size::BODY, r).mix(fm, sm).draw(&env, p);
            }
        }
        if scrolls {
            p.clip_clear();
        }
    })
}

/// Which colour treatment a control (Button / CircleButton) wears. One control widget, three looks —
/// the focus-driven default (every pill and disc in the app, the hero Play button included), a
/// caller-coloured one-off, and the keyline pill for a secondary action over video.
///
/// There used to be a fourth, `Primary`: an always-filled cool-white CTA. It went with
/// `theme::FILL_PRIMARY` in the 2026-08-13 palette sync — **nothing is filled by rank, only by
/// focus**, so a control that lights up while the remote is elsewhere is a lie about where you are,
/// and at ten feet its white was indistinguishable from [`theme::ACCENT`] anyway. It had no callers
/// by then; the hero Play pill has been `Accent` for months.
#[derive(Clone, Copy)]
pub enum ControlStyle {
    /// focus-driven: focused → ACCENT + dark ink; idle → solid dark disc + white ink. The
    /// default, and the shared look of the transport buttons / info-card actions / detail buttons.
    Accent,
    /// caller supplies the exact fill + ink.
    Custom { fill: [f32; 4], ink: [f32; 4] },
    /// The **keyline** pill — idle, a hairline outline ([`theme::PILL_KEYLINE`]) knocked out over
    /// a translucent near-black interior ([`theme::PILL_KEYLINE_BG`]), for a secondary action
    /// sitting on SCRIMMED VIDEO (the post-play card's "Watch credits"), where [`Accent`]'s solid
    /// idle plate reads as a hole in the picture. Focused it takes the standard Accent treatment,
    /// so focus reads identically across every control in the family.
    Keyline,
}
impl ControlStyle {
    /// (fill, ink) for this style at the given focus state.
    pub(crate) fn colors(self, focused: bool) -> ([f32; 4], [f32; 4]) {
        match self {
            ControlStyle::Accent if focused => (crate::ui::ACCENT, crate::ui::ACCENT_INK),
            ControlStyle::Accent => (theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_INK),
            ControlStyle::Custom { fill, ink } => (fill, ink),
            ControlStyle::Keyline if focused => (crate::ui::ACCENT, crate::ui::ACCENT_INK),
            ControlStyle::Keyline => (theme::PILL_KEYLINE_BG, theme::TEXT_HEADING),
        }
    }
}

// ---- Button: a pill with a label, an optional leading icon and an optional TRAILING accessory,
// centered together as one group (icon + gap + label + gap + accessory is centered in the pill).
// Colour per `ControlStyle` (default Accent). The one reusable action button — hero Play (Primary),
// detail/info actions (Accent), etc. ----
pub struct Button {
    pub frame: Rect,
    pub label: *const c_char,
    pub sz: c_int,
    pub icon: Option<crate::ui::icons::Icon>,
    /// The TRAILING accessory glyph — a chevron saying the press opens a list rather than acting
    /// ([`crate::ui::alt_sources`]'s *Also available*). Deliberately its own slot rather than a
    /// second use of [`Button::icon`]: the leading icon is part of the label's own statement (the
    /// Play triangle IS "play"), while this one is a disclosure mark about what the control DOES,
    /// and the two are read in opposite directions. It is the same `›`-family mark
    /// [`crate::ui::table::Row::ticon`] puts at a row's trailing edge, for the same reason.
    pub trailing: Option<crate::ui::icons::Icon>,
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
/// [`ControlStyle::Keyline`]'s stroke width (the design's 1.5 — same weight as [`keyline_chip`]'s).
const BTN_KEYLINE_W: f32 = 1.5;

impl Button {
    pub fn new(label: *const c_char, sz: c_int, frame: Rect) -> Self {
        Self { frame, label, sz, icon: None, trailing: None, focused: false, style: ControlStyle::Accent, progress: None }
    }
    pub fn icon(mut self, i: crate::ui::icons::Icon) -> Self {
        self.icon = Some(i);
        self
    }
    /// Give this pill a trailing accessory — see [`Button::trailing`].
    pub fn trailing_icon(mut self, i: crate::ui::icons::Icon) -> Self {
        self.trailing = Some(i);
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
        Self::pill_w_full(label, sz, icon, false)
    }

    /// [`Button::pill_w`] with the TRAILING accessory counted too — the same one formula, taking
    /// both of the button's optional slots rather than growing a second sizing path beside it (the
    /// drift this file exists to prevent, and the exact reason `pill_w` gained its `icon` flag).
    /// An accessory occupies one more icon box and one more gap, which is what [`Button::draw`]
    /// lays out below.
    pub fn pill_w_full(label: *const c_char, sz: c_int, icon: bool, trailing: bool) -> f32 {
        let (isz, gap) = if icon { (sz as f32 * BTN_ICON_RATIO, BTN_ICON_GAP) } else { (0.0, 0.0) };
        let (tsz, tgap) = if trailing { (sz as f32 * BTN_ICON_RATIO, BTN_ICON_GAP) } else { (0.0, 0.0) };
        isz + gap + crate::text::text_width(label, sz, 1) + tgap + tsz + BTN_PILL_AIR
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
        if matches!(self.style, ControlStyle::Keyline) && !self.focused {
            // the knockout: stroke colour first, then the interior inset by it — the SDF has no
            // stroke-only mode (`keyline_chip` / `pass_capsule`'s construction); `bg` here is
            // `colors()`'s translucent interior, not a repaint of the ground (there is none over
            // live video — see `theme::PILL_KEYLINE_BG`)
            p.rrect(r, rad, rad, theme::PILL_KEYLINE);
            let s = BTN_KEYLINE_W;
            p.rrect(Rect::new(r.x + s, r.y + s, r.w - 2.0 * s, r.h - 2.0 * s), rad - s, rad - s, bg);
        } else {
            p.rrect(r, rad, rad, bg);
        }
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
        let (asz, agap) =
            if self.trailing.is_some() { (self.sz as f32 * BTN_ICON_RATIO, BTN_ICON_GAP) } else { (0.0, 0.0) };
        // the WHOLE run is centred — accessory included — which is why `pill_w_full` measures it:
        // sizing the pill without the chevron and then drawing one would push the label off-centre
        let gl = r.cx() - (isz + gap + tw + agap + asz) * 0.5;
        if let Some(icon) = self.icon {
            crate::ui::icons::draw(p, icon, Rect::new(gl, r.y + (r.h - isz) * 0.5, isz, isz), ink);
        }
        p.text(self.label, gl + isz + gap, ty, self.sz, ink, 0, 1); // left-aligned after the icon
        if let Some(acc) = self.trailing {
            let ax = gl + isz + gap + tw + agap;
            crate::ui::icons::draw(p, acc, Rect::new(ax, r.y + (r.h - asz) * 0.5, asz, asz), ink);
        }
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
/// relationship [`rating_group`]'s verdict mark has to its score. Deliberately smaller than
/// [`Button`]'s `sz * BTN_ICON_RATIO`: this chip is half a button's height, so a button-proportioned
/// glyph would fill it edge to edge.
const BADGE_ICON: f32 = theme::size::CAPTION as f32;
/// Glyph → label air. They are one run, so the tightest rung (as in [`rating_group`]).
const BADGE_ICON_GAP: f32 = theme::space::XS;

/// pixel width [`badge`] will occupy for `text` (+ `icon`) — the layout companion (e.g. reserving
/// the inline-chip run so a row label elides before it). The icon's band is added OUTSIDE the
/// short-tag floor, so a bare "CC" still measures its minimum and a glyphed chip still fits both.
// ---- The PLEX PASS capsule (`Details Screen.dc.html` / `Player Screen.dc.html`) --------------
//
// The name set in type — deliberately NO logo artwork: this is an unofficial client, and the
// badge is a referential use of the words alone (the same reasoning `plex::identity` documents
// for the product name). Height matches [`BADGE_H`] so it shares a badge row's optical line.
// **Two product surfaces, both places the name changes what the user does next** (see
// `theme::PASS_GOLD`'s doc for the docs-derived rule): FILLED in the playback-failed read-out
// (pure black ground), OUTLINE in the detail facts row's Pass-gated states. Non-interactive in
// both; it is [`theme::PASS_GOLD`]'s only consumer.
//
// **Re-spec'd 2026-08-12** to the geometry BOTH mock files now carry (which is what makes it a
// decision rather than a drift): an 8px rounded rect at a 2px stroke, `size::CAPTION` bold at
// `.06em` — up from a full-pill silhouette, a 1.5px stroke and `size::MICRO` at `.12em`. The
// silhouette is no longer what separates it from a technical chip; the GOLD is, and the label
// now sits on the couch-legibility floor instead of below it. (The Player Screen's comment
// beside the filled form still says "full pill radius" against its own `border-radius:8px` —
// the CSS is the artifact and the comment is stale.)

/// Letter-tracking for the capsule label: the design's `.06em` of [`theme::size::CAPTION`]. The
/// text renderer has no letter-spacing, so the label is drawn per character.
const PASS_TRACK: f32 = theme::size::CAPTION as f32 * 0.06;
/// The label inset. The design states `padding: 0 13px 0 15px` and **that asymmetry does not
/// port** — honouring the reasoning, not the literal. The mock is asymmetric because CSS emits a
/// letter-space after the LAST character too, so it takes that trailing space off the right pad to
/// keep the label optically centred; its own comment says exactly that. Our renderer tracks
/// BETWEEN characters only ([`pass_label_w`] counts `n − 1` gaps), so there is no trailing space to
/// compensate for, and copying 15/13 would push the ink 1px right of centre — off-centre in the
/// opposite direction from the thing the design was correcting. 14/14 keeps the SUM, and therefore
/// the drawn width and every layout measured from it, byte-identical.
const PASS_PAD_X: f32 = 14.0;
/// Corner radius and stroke — `border-radius: 8px`, `inset 0 0 0 2px`.
const PASS_RAD: f32 = 8.0;
const PASS_STROKE: f32 = 2.0;
/// The label, "PLEX PASS", **pre-split into per-character `CStr` literals** — the tracking is
/// applied by advancing the pen between them, so the label is nine one-character draws.
/// Compile-time constants rather than nine `CString::new` allocations per call: `pass_capsule_w`
/// alone walks them, and the capsule is now on the detail hero for every converting item on a
/// proven-Pass-less server (not only an HDR one) as well as in the failure read-out, so this ran
/// ~18 small allocations a frame for a string that never changes.
const PASS_CHARS: [&std::ffi::CStr; 9] = [c"P", c"L", c"E", c"X", c" ", c"P", c"A", c"S", c"S"];

/// The label's own drawn width, **memoised** — the pens below re-measure per character anyway, so
/// only the total is worth holding. Main-thread only, like every other layout memo here.
static mut PASS_W: f32 = 0.0;

fn pass_label_w() -> f32 {
    // `text_width` reads 0 until `init_text` has run — never cache a pre-init measurement (the
    // same guard `ctrl_slot`'s width memo keeps, and for the same reason).
    let memo = unsafe { PASS_W };
    if memo > 0.0 {
        return memo;
    }
    let mut w = 0.0;
    for c in PASS_CHARS {
        w += crate::text::text_width(c.as_ptr(), theme::size::CAPTION, 1);
    }
    if w <= 0.0 {
        return 0.0;
    }
    w += PASS_TRACK * (PASS_CHARS.len() - 1) as f32;
    unsafe { PASS_W = w };
    w
}

/// Layout width of the capsule — for right-anchoring and row flow.
pub(crate) fn pass_capsule_w() -> f32 {
    pass_label_w() + 2.0 * PASS_PAD_X
}

/// Draw the capsule with its LEFT edge at `x`, centred on `cy`; returns its width.
///
/// `filled: false` is the OUTLINE form — a [`PASS_STROKE`] ring of pass-gold with a gold label and
/// **nothing inside it** ([`Painter::rring`]), which is the mock's `box-shadow: inset 0 0 0 2px
/// var(--pass-gold)` with no `background`. It is the default everywhere a surface sits behind it,
/// and it no longer has to be told what that surface is: the knockout it replaces painted the
/// interior in a `bg` the caller named, which over the detail hero's backdrop meant a gold-ringed
/// dark BOX rather than a hairline.
///
/// `filled: true` is the FILLED form — pass-gold fill, near-black label — used in exactly one
/// place, the playback-failed read-out, where the ground is pure black and an outline would read
/// as a hole.
pub(crate) fn pass_capsule(p: Painter, x: f32, cy: f32, filled: bool) -> f32 {
    let w = pass_capsule_w();
    let r = Rect::new(x, cy - BADGE_H * 0.5, w, BADGE_H);
    let ink = if filled {
        p.rrect(r, PASS_RAD, PASS_RAD, theme::PASS_GOLD);
        theme::PASS_GOLD_INK
    } else {
        p.rring(r, PASS_RAD, PASS_STROKE, theme::PASS_GOLD);
        theme::PASS_GOLD
    };
    let ty = crate::text::text_vcenter_y(theme::size::CAPTION, 1, cy);
    let mut cx = x + PASS_PAD_X;
    for c in PASS_CHARS {
        p.text(c.as_ptr(), cx, ty, theme::size::CAPTION, ink, 0, 1);
        cx += crate::text::text_width(c.as_ptr(), theme::size::CAPTION, 1) + PASS_TRACK;
    }
    w
}

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
// ---- Rating row: one PROVIDER's scores under the provider's name in words.
//
// Rewritten 2026-08-02 from `Details Screen.dc.html`. It used to draw one badge per score, each
// behind that provider's own brand mark — Rotten Tomatoes' fruit and popcorn tub as tinted
// silhouettes, IMDb and TMDB as logotype chips in their brand colours. All of that is gone:
//
//   * the RT marks had no licensing route (see `ui/icons.rs`), and
//   * the chips were reproducing two more brands' logotypes to solve a problem — "whose score is
//     this?" — that a WORD solves for free, and that naming the provider solves *lawfully*, since
//     referential use needs no licence where a mark does.
//
// So a group is: the provider's name as a quiet MICRO caption in TEXT_TERTIARY, then its score or
// scores. That inverts what carried the colour. Before, four saturated brand marks competed with
// the hero art and with each other; now the captions recede to caption weight and the ONLY colour
// left in the row is the verdict — a red or gold or hollow tomato, a green or drained crowd. The
// row reads as one rhythm instead of four logos.
//
// Rotten Tomatoes is ONE group with two scores under one caption, because critics and audience are
// two readings from one source; IMDb and TMDB are one score each. That is also why this draws a
// GROUP rather than a badge: the caption is shared, so the unit that knows how to lay itself out
// is the provider, not the score.
// ----

/// Mark box (px). A little over the meta line's cap height so a 26-unit silhouette still resolves
/// at couch distance — these marks carry the VERDICT, so legibility here is not cosmetic.
const RATING_MARK_D: f32 = 30.0;
/// Glyph → its score. They are one unit, so it stays tight.
const RATING_GAP: f32 = 10.0;
/// Provider caption → the first score under it.
const RATING_CAPTION_GAP: f32 = 12.0;
/// Score → the next glyph in the SAME group (Rotten Tomatoes' critic → audience). Wider than
/// [`RATING_GAP`] so the two pairs read as two readings rather than one run of four things.
const RATING_PAIR_GAP: f32 = 14.0;

/// One colour layer of a rating mark — a mask and the tint it is painted in. Marks are two-tone
/// (body + calyx), and the rasterizer renders a MASK, so a mark is a slice rather than one icon.
pub(crate) type MarkLayer = (crate::ui::icons::Icon, [f32; 4]);

/// One score inside a provider group: the mark that carries its verdict, and the score as text.
/// `mark` is empty for a provider that has no verdict to draw (IMDb, TMDB) — their number IS the
/// whole statement, and inventing a glyph for them is what put a meaningless star here before.
pub(crate) struct RatingCell<'a> {
    pub(crate) mark: &'a [MarkLayer],
    pub(crate) value: &'a str,
    /// Trailing unit set a rung down in tertiary ink — IMDb's "/10". A percentage carries its own
    /// "%" inside `value`, because there the unit is part of the number rather than a scale note.
    pub(crate) suffix: &'a str,
}

/// Width [`rating_group`] will occupy. Measure before drawing so a row can stop at a margin
/// instead of running a group off the panel (same contract as `badge`/`badge_w`).
pub(crate) fn rating_group_w(caption: &str, cells: &[RatingCell]) -> f32 {
    let cap = std::ffi::CString::new(caption).ok();
    let mut w = match cap {
        Some(c) => crate::text::text_width(c.as_ptr(), theme::size::MICRO, 1) + RATING_CAPTION_GAP,
        None => return 0.0,
    };
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            w += RATING_PAIR_GAP;
        }
        if !cell.mark.is_empty() {
            w += RATING_MARK_D + RATING_GAP;
        }
        if let Ok(v) = std::ffi::CString::new(cell.value) {
            w += crate::text::text_width(v.as_ptr(), theme::size::LABEL, 1);
        }
        if let Ok(s) = std::ffi::CString::new(cell.suffix) {
            w += crate::text::text_width(s.as_ptr(), theme::size::MICRO, 1);
        }
    }
    w
}

/// Draw one provider's group with its LEFT edge at `x`, centred on `cy`; returns its width.
pub(crate) fn rating_group(p: Painter, x: f32, cy: f32, caption: &str, cells: &[RatingCell]) -> f32 {
    let Ok(cap) = std::ffi::CString::new(caption) else { return 0.0 };
    let mut bx = x;
    // The caption sits on the SCORE's baseline, not on its own centre: the design aligns the row
    // by baseline (`align-items:baseline`), so a MICRO caption beside a LABEL number must share
    // the number's baseline or it floats. `text::baseline_y` is that rule, shared.
    let base = crate::text::baseline_y(
        theme::size::MICRO,
        1,
        theme::size::LABEL,
        1,
        crate::text::text_vcenter_y(theme::size::LABEL, 1, cy),
    );
    p.text(cap.as_ptr(), bx, base, theme::size::MICRO, theme::TEXT_TERTIARY, 0, 1);
    bx += crate::text::text_width(cap.as_ptr(), theme::size::MICRO, 1) + RATING_CAPTION_GAP;

    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            bx += RATING_PAIR_GAP;
        }
        if !cell.mark.is_empty() {
            // every layer rides the SAME rect, so all of them rasterize at one size from one
            // viewBox and register exactly — see `ui/icons.rs`'s note on the layered marks
            let r = Rect::new(bx, cy - RATING_MARK_D * 0.5, RATING_MARK_D, RATING_MARK_D);
            for (mask, tint) in cell.mark.iter() {
                crate::ui::icons::draw(p, *mask, r, *tint);
            }
            bx += RATING_MARK_D + RATING_GAP;
        }
        if let Ok(v) = std::ffi::CString::new(cell.value) {
            let ty = crate::text::text_vcenter_y(theme::size::LABEL, 1, cy);
            p.text(v.as_ptr(), bx, ty, theme::size::LABEL, theme::TEXT_PRIMARY, 0, 1);
            bx += crate::text::text_width(v.as_ptr(), theme::size::LABEL, 1);
        }
        if let Ok(s) = std::ffi::CString::new(cell.suffix) {
            p.text(s.as_ptr(), bx, base, theme::size::MICRO, theme::TEXT_TERTIARY, 0, 1);
            bx += crate::text::text_width(s.as_ptr(), theme::size::MICRO, 1);
        }
    }
    // the MEASURED width, not what the draws accumulated — a draw and its measurer that can
    // disagree will eventually be caught disagreeing
    rating_group_w(caption, cells)
}


#[cfg(test)]
mod tests {
    use super::*;

    // ── The poster's state mark: one vocabulary, one mark at a time ───────────────────────────
    //
    // `card` needs a GL context, so the CHOICE is what a host test can reach — and the choice is
    // where this has been wrong before: the mark used to say "unwatched" in amber, which is the
    // opposite claim, and the bar/disc precedence is only observable on an item PMS reports as both.

    /// A MOVIE row at a given watched state. `dur_ns` is 100 min, so `resume_ms` reads as a
    /// percentage of the way in. For a LEAF the two flags really are each other's negation — which
    /// is exactly what stops being true for a container, hence the show cases below.
    fn row(watched: bool, resume_ms: i64) -> PmsMovie {
        let mut m = PmsMovie::default();
        m.dur_ns = 100 * 60 * 1000 * 1_000_000;
        m.watched = watched;
        m.unwatched = !watched;
        m.resume_ms = resume_ms;
        m
    }

    #[test]
    fn a_poster_nobody_has_started_wears_no_mark_at_all() {
        // the common case on any real server, and the whole reason the polarity inverted
        assert_eq!(poster_mark(&row(false, 0)), PosterMark::None);
    }

    #[test]
    fn a_finished_poster_wears_the_watched_disc() {
        assert_eq!(poster_mark(&row(true, 0)), PosterMark::Watched);
    }

    #[test]
    fn a_re_watch_in_flight_outranks_the_watched_flag() {
        // PMS reports BOTH on a finished-then-restarted item; the bar wins, so the tile never
        // wears two marks — and what it says is what the viewer is actually doing.
        let m = row(true, 30 * 60 * 1000);
        assert_eq!(poster_mark(&m), PosterMark::InProgress);
        assert!(m.resume_frac().is_some(), "InProgress must be exactly when the caller draws the bar");
    }

    #[test]
    fn a_part_watched_poster_that_was_never_finished_is_in_progress_too() {
        assert_eq!(poster_mark(&row(false, 30 * 60 * 1000)), PosterMark::InProgress);
    }

    #[test]
    fn an_offset_the_server_never_cleared_is_finished_not_in_progress() {
        // resume AT or PAST the end: a full-width bar there read as a rendering bug, and it would
        // now also hide the disc the item has earned. Both the mark and the bar must agree.
        for resume in [100 * 60 * 1000, 200 * 60 * 1000] {
            let m = row(true, resume);
            assert_eq!(m.resume_frac(), None, "resume {resume} must not draw a bar");
            assert_eq!(poster_mark(&m), PosterMark::Watched, "resume {resume}");
        }
    }

    #[test]
    fn a_row_with_no_runtime_cannot_be_in_progress() {
        // dur_ns == 0 (the server sent no duration): a fraction is undefined, so there is no bar to
        // draw and the watched flag alone decides.
        let mut m = row(true, 30 * 60 * 1000);
        m.dur_ns = 0;
        assert_eq!(m.resume_frac(), None);
        assert_eq!(poster_mark(&m), PosterMark::Watched);
        let mut m = row(false, 30 * 60 * 1000);
        m.dur_ns = 0;
        assert_eq!(poster_mark(&m), PosterMark::None);
    }

    #[test]
    fn a_show_three_episodes_in_is_not_a_watched_show() {
        // The device capture that caught this: a library filtered to `unwatchedLeaves=1` — every
        // tile has an unseen episode by construction — had five posters wearing a watched disc,
        // because `!unwatched` is true for a container the moment ONE episode is played. A show is
        // marked only when it is DONE, so partly-watched sits with never-started under "no mark".
        let mut m = PmsMovie::default();
        m.kind = 1; // show
        m.unwatched = false; // some episode has been played…
        m.watched = false; // …but not all of them
        assert_eq!(poster_mark(&m), PosterMark::None);
        m.watched = true;
        assert_eq!(poster_mark(&m), PosterMark::Watched, "every leaf seen IS the disc");
    }

    // ── AmbientWash: the page GROUND's legibility contract ───────────────────────────────────
    //
    // All pure math over `theme` tokens — no GL, no globals, so these are ordinary parallel tests.
    // `Spring::step` (used by the dissolve test) is `gfx::spring`, closed-form arithmetic.

    /// One sRGB channel, linearized (WCAG 2.x). Local to the tests on purpose: the app never needs
    /// this — it is the yardstick the design decision was made with, kept here so the decision stays
    /// checkable rather than remembered.
    fn linearize(c: f32) -> f32 {
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }
    /// WCAG relative luminance of an rgba token (alpha ignored — a ground is opaque).
    fn rel_luma(c: [f32; 4]) -> f32 {
        0.2126 * linearize(c[0]) + 0.7152 * linearize(c[1]) + 0.0722 * linearize(c[2])
    }
    /// WCAG contrast ratio between two opaque colours, brighter over darker.
    fn contrast(a: [f32; 4], b: [f32; 4]) -> f32 {
        let (x, y) = (rel_luma(a), rel_luma(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }
    /// The corner sources a ground has to survive: the blown-out extreme, each primary and secondary
    /// at full saturation, a mid grey, and the brightest thing in the palette.
    fn hostile_sources() -> Vec<[f32; 3]> {
        vec![
            [1.0, 1.0, 1.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [1.0, 0.0, 1.0],
            [0.5, 0.5, 0.5],
            [theme::WASH_WARM[0], theme::WASH_WARM[1], theme::WASH_WARM[2]],
            [theme::TEXT_PRIMARY[0], theme::TEXT_PRIMARY[1], theme::TEXT_PRIMARY[2]],
        ]
    }

    /// **The legibility contract, executable.** Every corner of an artwork-keyed ground at
    /// [`AmbientWash::GROUND_W`] must clear 3:1 against [`theme::TEXT_TERTIARY`] — the dimmest ink
    /// the app puts on a ground (cast roles, unfocused episode summaries, the About column's labels,
    /// all at `size::CAPTION`) — and 7:1 against [`theme::TEXT_PRIMARY`]. This is the test that would
    /// have caught the UNCAPPED version (a white corner lands tertiary at 1.98:1), and it is the one
    /// that fails the day someone raises `GROUND_W` or `GROUND_LUMA` past what a page can carry. It
    /// guards the person page and the detail page at once, because both go through `keyed`.
    #[test]
    fn a_ground_never_outshines_the_fine_print_that_sits_on_it() {
        for src in hostile_sources() {
            let g = AmbientWash::keyed([src; 4], [AmbientWash::GROUND_W; 4]);
            for (i, corner) in g.iter().enumerate() {
                let t = contrast(theme::TEXT_TERTIARY, *corner);
                assert!(t >= 3.0, "corner {i} of {src:?}: TEXT_TERTIARY at {t:.2}:1, under the 3:1 large-text floor");
                let p = contrast(theme::TEXT_PRIMARY, *corner);
                assert!(p >= 7.0, "corner {i} of {src:?}: TEXT_PRIMARY at {p:.2}:1");
            }
        }
    }

    /// The other end: a ground keyed to near-black key art must not sink below the value a card's
    /// drop shadow needs to read against. `SURFACE_APP`'s own doc rejects the old near-black
    /// (25,25,29)/255 for exactly that reason; `GROUND_W ≤ 0.26` is what keeps the floor above it,
    /// with no floor constant anywhere.
    #[test]
    fn a_ground_stays_light_enough_for_a_card_shadow() {
        let floor = AmbientWash::keyed([[0.0; 3]; 4], [AmbientWash::GROUND_W; 4]);
        let rejected = [25.0 / 255.0, 25.0 / 255.0, 29.0 / 255.0, 1.0];
        for corner in floor {
            for ch in 0..3 {
                let want = theme::SURFACE_APP[ch] * (1.0 - AmbientWash::GROUND_W);
                assert!((corner[ch] - want).abs() < 1e-6, "the darkest ground is the surface scaled by 1-GROUND_W");
                assert!(corner[ch] > rejected[ch], "channel {ch} sank to {} — into the near-black the palette rejects", corner[ch]);
            }
        }
    }

    /// "No artwork is the app's own flat ground" — stated in the type's docs since the person page
    /// shipped, pinned here. At weight 0 the mix must be EXACTLY the surface, so a screen needs no
    /// has-envelope branch in its draw.
    #[test]
    fn no_artwork_is_the_apps_own_ground() {
        for src in hostile_sources() {
            let quad = [[src[0], src[1], src[2], 1.0]; 4];
            assert_eq!(AmbientWash::target(quad, [0.0; 4]), [theme::SURFACE_APP; 4], "src {src:?}");
        }
    }

    /// The cap is a scalar multiply, not a per-channel clamp: a bright saturated corner comes back
    /// at exactly [`GROUND_LUMA`] with its channel RATIOS untouched (a clamp would desaturate it
    /// toward white), and a corner already under the ceiling comes back bit-identical.
    #[test]
    fn the_luma_cap_spends_brightness_and_nothing_else() {
        let bright = [0.9, 0.7, 0.2];
        let c = ground_capped(bright);
        let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        assert!((y - GROUND_LUMA).abs() < 1e-4, "capped luma {y} != {GROUND_LUMA}");
        let k = c[0] / bright[0];
        for ch in 0..3 {
            assert!((c[ch] / bright[ch] - k).abs() < 1e-5, "channel {ch} was scaled by a different factor — that is a clamp, not a cap");
        }
        assert_eq!(c[3], 1.0, "a ground corner is opaque");

        let dark = [0.10, 0.20, 0.05];
        assert_eq!(ground_capped(dark), [dark[0], dark[1], dark[2], 1.0], "a corner under the ceiling is untouched");
    }

    /// A tl/tr/br/bl transposition is invisible on a near-symmetric gradient in a screenshot and
    /// wrong on every asymmetric one — the exact hazard `UltraBlurColors::corners`' doc warns about
    /// (the JSON reading order is not the ring order). Both constructors must be index-preserving.
    #[test]
    fn the_corners_keep_their_ring_order() {
        let src = [[0.8, 0.1, 0.1], [0.1, 0.8, 0.1], [0.1, 0.1, 0.8], [0.7, 0.7, 0.1]];
        let w = [AmbientWash::GROUND_W; 4];
        let keyed = AmbientWash::keyed(src, w);
        for i in 0..4 {
            let alone = AmbientWash::keyed([src[i]; 4], w);
            assert_eq!(keyed[i], alone[0], "corner {i} did not stay at index {i}");
        }
        let quad: [[f32; 4]; 4] = std::array::from_fn(|i| [src[i][0], src[i][1], src[i][2], 1.0]);
        let plain = AmbientWash::target(quad, w);
        for i in 0..4 {
            assert_eq!(plain[i], theme::mix(theme::SURFACE_APP, quad[i], w[i]), "target moved corner {i}");
        }
    }

    /// The skip test a screen uses to avoid a full-screen fill that changes nothing: flat on the
    /// ground it is drawn over → skippable; dissolving toward a different colour → not; settled back
    /// → skippable again. Drives the real corner springs, so it also pins that a dissolve actually
    /// converges within the ~0.5 s the rate promises.
    #[test]
    fn a_wash_that_has_resolved_to_the_ground_is_flat() {
        const EPS: f32 = AmbientWash::FLAT_EPS;
        let mut w = AmbientWash::flat(theme::SURFACE_APP);
        assert!(w.is_flat(theme::SURFACE_APP, EPS), "a wash mounted on the ground is already flat");

        let away = [[0.9, 0.2, 0.1, 1.0]; 4];
        for _ in 0..6 {
            w.step(away, AmbientWash::K, 1.0 / 60.0);
        }
        assert!(!w.is_flat(theme::SURFACE_APP, EPS), "a wash on its way to a colour is not the ground");

        for _ in 0..60 {
            w.step([theme::SURFACE_APP; 4], AmbientWash::K, 1.0 / 60.0);
        }
        assert!(w.is_flat(theme::SURFACE_APP, EPS), "a second of dissolve must land back on the ground");
    }

    // ── The hero corner scrim: the wedge's shape, its seam, and the legibility it promises ──────
    //
    // All pure math over `theme` tokens and the two screens' own layout arithmetic — no GL, no
    // globals, so these are ordinary parallel tests. The screens' contributions come from
    // `home::base_scrim_a` / `detail::base_scrim_a` / `detail::hero_chain`, which is the whole
    // point of those three being pure: the contract below reads the SAME numbers the draw does.

    /// The bilinear field a `(rect, [tl, tr, br, bl])` quad actually rasterizes, at absolute
    /// `(x, y)` — the shader's own `mix(mix(tl,tr,u), mix(bl,br,u), v)`, in alpha. The anti-drift
    /// yardstick: the closed-form [`hero_scrim_a`]/[`hero_scrim_right_a`] the contract is graded on
    /// and the corner colours that are actually drawn must agree, or the promise is about a field
    /// nobody paints.
    fn bilerp_a(q: (Rect, [[f32; 4]; 4]), x: f32, y: f32) -> f32 {
        let (r, k) = q;
        let u = ((x - r.x) / r.w).clamp(0.0, 1.0);
        let v = ((y - r.y) / r.h).clamp(0.0, 1.0);
        let top = k[0][3] + (k[1][3] - k[0][3]) * u;
        let bot = k[3][3] + (k[2][3] - k[3][3]) * u;
        top + (bot - top) * v
    }

    /// The wedge is a DARKENER that peaks in the corner the text is in: monotone non-increasing in
    /// x, at full [`theme::SCRIM_TEXT_A`] at the margin, and exactly 0 from [`HERO_SCRIM_W`] out —
    /// a non-zero value there would draw a vertical line across the hero where the quads end. Also
    /// pins that every out-of-range input clamps rather than going negative or past 1: `x` and
    /// `strength` both come from live animation state, and a negative alpha here would BRIGHTEN
    /// the artwork under the copy.
    #[test]
    fn the_wedge_never_brightens_toward_the_text() {
        for &s in &[0.25f32, 0.5, 1.0] {
            assert!((hero_scrim_a(0.0, s) - theme::SCRIM_TEXT_A * s).abs() < 1e-6, "the margin is the peak");
            let mut prev = f32::INFINITY;
            let mut x = 0.0f32;
            while x <= crate::ui::consts::SCR_W {
                let a = hero_scrim_a(x, s);
                assert!(a <= prev + 1e-6, "s={s}: alpha rose from {prev} to {a} at x={x}");
                assert!((0.0..=1.0).contains(&a), "s={s} x={x}: alpha {a} outside 0..=1");
                prev = a;
                x += 8.0;
            }
            assert_eq!(hero_scrim_a(HERO_SCRIM_W, s), 0.0, "the wedge must reach exactly nothing at its end");
            assert_eq!(hero_scrim_a(crate::ui::consts::SCR_W, s), 0.0, "…and stay there");
            assert_eq!(hero_scrim_a(-40.0, s), theme::SCRIM_TEXT_A * s, "x left of the frame is the peak, not more");
        }
        assert_eq!(hero_scrim_a(0.0, -0.5), 0.0, "a negative strength is nothing, never an inverted wedge");
        assert_eq!(hero_scrim_a(0.0, 4.0), theme::SCRIM_TEXT_A, "strength saturates at the token");
    }

    /// **The seam** — the one structural bug this component can have, and invisible on the host in
    /// every other form. The two left quads must share an EXACT float y and an identical colour
    /// pair along it: a gap leaves one row of unscrimmed artwork (BRIGHT) straight across the
    /// hero, an overlap composites two scrims on one row (BLACK). Neither is subtle and both look
    /// like a renderer bug rather than a layout one.
    ///
    /// `art_scrim`'s hairline had a different cause (an integer-truncated scissor meeting a float
    /// fill) and a different fix (`gfx::snap`). This pair is fill-to-fill: do not "fix" it by
    /// snapping, which would move the seam off the shared float and CREATE the bug.
    #[test]
    fn the_wedges_two_quads_abut_with_no_step() {
        let (q, n) = hero_scrim_quads(1.0, false);
        assert_eq!(n, 2);
        let (r0, k0) = q[0];
        let (r1, k1) = q[1];
        assert_eq!(r0.y + r0.h, r1.y, "the seam is not one float");
        assert_eq!(r0.x, r1.x, "the two quads must be the same column");
        assert_eq!(r0.w, r1.w);
        assert_eq!(k0[3], k1[0], "quad 0's bottom-left must be quad 1's top-left");
        assert_eq!(k0[2], k1[1], "quad 0's bottom-right must be quad 1's top-right");
        assert_eq!(r1.y + r1.h, crate::ui::consts::SCR_H, "the wedge runs to the foot of the panel");
        // …and the field is continuous ACROSS the seam, not merely the same colour at its ends:
        // sample both quads' own bilinear along it.
        for x in [0.0f32, 300.0, 900.0, HERO_SCRIM_W] {
            let below = bilerp_a(q[1], x, r1.y);
            assert!((bilerp_a(q[0], x, r0.y + r0.h) - below).abs() < 1e-6, "step at x={x}");
            // The quad's CORNERS are `hero_scrim_a` by construction now, so what is left to grade in
            // the interior is that the closed form is AFFINE in x — a curve there would be a field
            // `grad4`'s straight interpolation cannot reproduce, and the contract would be about a
            // shape nobody paints.
            assert!((below - hero_scrim_a(x, 1.0)).abs() < 1e-6, "the drawn field disagrees with hero_scrim_a at x={x}");
        }
    }

    /// The top chrome owns its own legibility with its own dark capsule (`draw_tab_row`'s track).
    /// A wedge creeping up into that band is two treatments fighting over one strip — and the
    /// capsule was tuned assuming nothing else is under it.
    #[test]
    fn the_wedge_leaves_the_top_chrome_alone() {
        let (q, _) = hero_scrim_quads(1.0, true);
        let bar_bottom = TOP_BAR_Y + TAB_PILL_H + TAB_TRACK_PAD + theme::space::XS;
        for (i, (r, _)) in q.iter().enumerate() {
            assert!(r.y >= bar_bottom, "quad {i} starts at y={} — inside the top chrome's band ({bar_bottom})", r.y);
        }
        // The feather also has to start ABOVE the highest text anchor, which is what lets
        // `hero_scrim_a` be a function of x alone (see `HERO_SCRIM_KNEE`) and is what makes the
        // legibility table below sound — every row it grades is at or below this line. A clearLogo
        // paints higher than any of them, but that is ART riding the feather, not a graded anchor.
        let highest = detail_title_cap_top().min(home_title_cap_top());
        assert!(HERO_SCRIM_KNEE <= highest, "the knee ({HERO_SCRIM_KNEE}) must be at or above the topmost hero line ({highest})");
    }

    /// The right wedge is OPT-IN (home's hero has no right-aligned copy and must not pay for one)
    /// and feathers in from both of its inner edges, so neither boundary can draw a line on the
    /// picture. Its overlap with the left wedge's tail is intended, not an accident to "fix" by
    /// moving an edge: the two fields multiply, and the facts line lives in the overlap.
    #[test]
    fn the_right_wedge_is_opt_in_and_starts_at_zero() {
        assert_eq!(hero_scrim_quads(1.0, false).1, 2, "no right wedge unless asked for");
        let (q, n) = hero_scrim_quads(1.0, true);
        assert_eq!(n, 3);
        let (r, k) = q[2];
        assert_eq!([k[0][3], k[1][3], k[3][3]], [0.0, 0.0, 0.0], "only the bottom-right corner carries weight");
        assert_eq!(k[2][3], HERO_SCRIM_R_A, "…and it carries exactly the token");
        for y in [r.y, r.y + r.h * 0.5, r.y + r.h] {
            assert_eq!(bilerp_a(q[2], r.x, y), 0.0, "the left edge must not draw a line");
        }
        for x in [r.x, r.x + r.w * 0.5, r.x + r.w] {
            assert_eq!(bilerp_a(q[2], x, r.y), 0.0, "the top edge must not draw a line");
        }
        // …and inside the quad the closed form must be the BILINEAR product `grad4` can actually
        // interpolate between those corners (u·v). The corners themselves are `hero_scrim_right_a`
        // by construction; this is the part of the shape that construction does not pin.
        for x in [1250.0f32, 1500.0, 1830.0, 1920.0] {
            for y in [750.0f32, 900.0, 1080.0] {
                let want = hero_scrim_right_a(x, y, 1.0);
                assert!((bilerp_a(q[2], x, y) - want).abs() < 1e-6, "drawn field != hero_scrim_right_a at ({x},{y})");
            }
        }
        assert!(r.x < HERO_SCRIM_W, "the two wedges are meant to overlap — the facts line sits in the overlap");
    }

    /// The wedge belongs to the HERO, not to the frame: at strength 0 there is nothing to draw.
    /// A residual wedge on the grid/shelf view would be a real regression and is invisible in a
    /// still — the shelves need the flat ground their card drop-shadows are tuned against.
    ///
    /// Graded on the closed forms alone. The quads' own corners USED to be swept here too, and that
    /// sweep is now a tautology: [`hero_scrim_quads`] builds every corner by evaluating these two
    /// functions, so it cannot report weight they do not have.
    #[test]
    fn the_wedge_leaves_with_the_hero() {
        assert_eq!(hero_scrim_a(0.0, 0.0), 0.0);
        assert_eq!(hero_scrim_a(HERO_SCRIM_W, 0.0), 0.0);
        assert_eq!(hero_scrim_right_a(1920.0, 1080.0, 0.0), 0.0);
    }

    // ---- the legibility contract itself -------------------------------------------------------

    /// One sRGB channel, linearized (WCAG 2.x) — see the `AmbientWash` block above for why this
    /// yardstick lives in the tests rather than in the app.
    fn srgb_lin(c: f32) -> f32 {
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }
    /// The contrast ratio an ink gets over `art` (a flat encoded grey standing in for a backdrop)
    /// once a scrim of total alpha `a` is composited over it. The panel is plain 888 with no sRGB
    /// framebuffer anywhere in the tree, so token values ARE sRGB codes and GL's blend is a
    /// straight lerp in that space: `c = art·(1−a) + SCRIM_INK·a`.
    fn contrast_over_art(ink: [f32; 4], art: f32, a: f32) -> f32 {
        let comp: Vec<f32> = (0..3).map(|i| art * (1.0 - a) + theme::SCRIM_INK[i] * a).collect();
        let l1 = 0.2126 * srgb_lin(ink[0]) + 0.7152 * srgb_lin(ink[1]) + 0.0722 * srgb_lin(ink[2]);
        let l2 = 0.2126 * srgb_lin(comp[0]) + 0.7152 * srgb_lin(comp[1]) + 0.0722 * srgb_lin(comp[2]);
        (l1.max(l2) + 0.05) / (l1.min(l2) + 0.05)
    }

    /// One HERO-72 bold cap band, the device font's — quoted, not measured, because the host suite
    /// opens no SDL_ttf (the same boundary that keeps `detail::hero_chain` pure).
    const HERO_CAP_H: f32 = 52.0;

    /// The TOP of home's title band, from the bottom-anchored stack the hero draws (`hero_content`):
    /// the reserved logo band + `space::MD` + a one-line BODY kicker + `space::SM` + three MICRO
    /// synopsis lines, stacked UP from `HERO_TEXT_BOTTOM` through the screen's own `hero_stack_top`,
    /// so the contract reads the arithmetic the draw uses. The two TEXT heights are quoted like
    /// [`HERO_CAP_H`]; if the hero's rungs or leading change, these move with them.
    fn home_title_band_top() -> f32 {
        const META_H: f32 = 34.0; // one line of `size::BODY`
        const SYN_H: f32 = 87.0; // three `size::MICRO` lines at the hero's 29px leading
        crate::ui::home::hero_stack_top(
            crate::ui::hero_logo::band_h(crate::ui::hero_logo::LogoRung::Hero),
            META_H,
            theme::space::SM + SYN_H,
        )
    }

    /// …and the cap top of the TEXT it falls back to, which is what the legibility table grades.
    /// The fallback is BASELINED on the band's bottom edge (`hero_logo::place`'s optical rule), so
    /// the ink sits a cap band above that line, ~70px below the band's own top. A clearLogo occupies
    /// the band instead and reaches higher — that is art, graded by eye on a capture, not here.
    fn home_title_cap_top() -> f32 {
        home_title_band_top() + crate::ui::hero_logo::band_h(crate::ui::hero_logo::LogoRung::Hero) - HERO_CAP_H
    }

    /// The same line on the detail page, where the band's anchor is `TITLE_BOTTOM` outright.
    fn detail_title_cap_top() -> f32 {
        crate::ui::detail::TITLE_BOTTOM - HERO_CAP_H
    }

    /// **The prize: "is the hero readable" as an assertion instead of a judgement on a television.**
    ///
    /// Every line of hero copy either screen draws over artwork, at the WORST point of its run (the
    /// right end, where the wedge is weakest), graded as
    /// `1 − (1 − base_scrim_a) · (1 − hero_scrim_a) · (1 − hero_scrim_right_a)` against two
    /// backdrops: a bright one (encoded 0.85 — a blown highlight, brighter than essentially any
    /// fanart pixel) and a blown-white one (1.0, the degenerate case the design only promises a
    /// floor for).
    ///
    /// The ys are READ from the screens' own layout arithmetic — `hero_chain` for detail, the
    /// bottom-anchored stack for home — so a layout change updates this contract rather than
    /// silently invalidating it. The floors are the measured achieved ratios: the point is that
    /// this fires the day someone brightens `SCRIM_INK`, dims a text token, moves
    /// `HERO_TEXT_BOTTOM` or trims `HERO_SCRIM_W`, none of which is visible in a diff.
    ///
    /// The two TITLE rows grade the TEXT fallback, at its cap top — a clearLogo occupies that band
    /// instead on most items and paints higher than the knee, which is art riding the feather rather
    /// than an anchor this closed form can speak for (see [`HERO_SCRIM_KNEE`]).
    ///
    /// The design floor is 3:1 over bright art. ONE row misses it and is listed anyway rather than
    /// hidden: detail's **facts line** is `TEXT_TERTIARY` (L=0.317), which needs α≈0.79 for 4.5:1 —
    /// an essentially black corner. That is an INK decision, not a scrim one, and is deliberately
    /// deferred; when the facts line moves to `TEXT_SECONDARY` over artwork its floor becomes 3.0
    /// like the rest.
    ///
    /// The people column was the second such row for exactly one day. Bottom-anchoring it moved its
    /// worst case 74px up the frame, where the composite ground is ≈0.59 instead of ≈0.71, and at
    /// tertiary that is 2.37:1 — so the row's floor was written down as 2.35 to match. **A contract
    /// is not a measurement**: the entry below asserts the ordinary 3.0/2.5 again, and
    /// `detail::PEOPLE_INK` is what meets it (one ink step, no scrim retune — the arithmetic for
    /// why that is the cheap half of the trade is on that const).
    #[test]
    fn the_hero_text_reads_over_bright_artwork() {
        use crate::ui::consts::{MARGIN_X, SCR_W};
        let hc = crate::ui::detail::hero_chain(
            // a two-line blurb — the shape `hero_chain`'s own doc is tuned on. Measuring is the one
            // thing the host cannot do, so the height is quoted, not computed.
            76.0, true,
        );
        let band = crate::ui::hero_logo::band_h(crate::ui::hero_logo::LogoRung::Hero);
        let home_col_r = MARGIN_X + crate::ui::home::HERO_COL_W; // 750 — the column's right end
        let det_col_r = MARGIN_X + crate::ui::detail::HERO_TEXT_W; // 990 — the synopsis' wrap edge,
        // and since nit 2 the title band's column too: a very wide wordmark runs the same 990.
        let people_r = SCR_W - MARGIN_X; // 1830 — the right-aligned people column's own edge

        // (label, x, y, ink, right wedge?, floor over BRIGHT art, floor over BLOWN WHITE)
        let rows: [(&str, f32, f32, [f32; 4], bool, f32, f32); 9] = [
            ("home title (right end)", home_col_r, home_title_cap_top(), theme::TEXT_PRIMARY, false, 3.0, 2.5),
            ("home title (at the margin)", MARGIN_X, home_title_cap_top(), theme::TEXT_PRIMARY, false, 7.0, 6.0),
            ("home kicker", 500.0, home_title_band_top() + band + theme::space::MD, theme::TEXT_SECONDARY, false, 3.0, 2.5),
            ("home synopsis", home_col_r, crate::ui::home::HERO_TEXT_BOTTOM - 87.0, theme::TEXT_SECONDARY, false, 3.0, 2.5),
            ("detail title", det_col_r, detail_title_cap_top(), theme::TEXT_PRIMARY, true, 3.0, 2.5),
            ("detail meta", 700.0, hc.meta_y, theme::TEXT_SECONDARY, true, 3.0, 2.5),
            ("detail synopsis", det_col_r, hc.syn_y, theme::TEXT_READING, true, 3.0, 2.5),
            // ⚠ the deferred ink decision — see the doc above
            ("detail facts", 1270.0, hc.facts_y, theme::TEXT_TERTIARY, true, 2.6, 2.1),
            // The people column at its WORST case: the top line of the tallest block it can produce
            // (a wrapped credit over a wrapped cast list), `PEOPLE_MAX_LINES` above the buttons —
            // 74px higher than the old top-anchored block, where the right wedge is feathering in
            // from y=702 and supplies about half the alpha it did (0.092 against 0.178). It clears
            // the ordinary floor because its ink is `detail::PEOPLE_INK`, which is read from the
            // screen rather than restated here, so an ink change here is a test failure and not a
            // silent one.
            (
                "detail people (top line)",
                people_r,
                crate::ui::detail::people_top(hc.btn_y, crate::ui::detail::PEOPLE_MAX_LINES),
                crate::ui::detail::PEOPLE_INK,
                true,
                3.0,
                2.5,
            ),
        ];

        for (label, x, y, ink, right, min_bright, min_white) in rows {
            let base = if label.starts_with("home") {
                crate::ui::home::base_scrim_a(y, 1.0)
            } else {
                crate::ui::detail::base_scrim_a(y, 1.0)
            };
            let wedge = hero_scrim_a(x, 1.0);
            let rw = if right { hero_scrim_right_a(x, y, 1.0) } else { 0.0 };
            let total = 1.0 - (1.0 - base) * (1.0 - wedge) * (1.0 - rw);
            for (art, floor) in [(0.85f32, min_bright), (1.0, min_white)] {
                let before = contrast_over_art(ink, art, base);
                let after = contrast_over_art(ink, art, total);
                assert!(
                    after >= floor,
                    "{label} at ({x},{y}) over art {art}: {after:.2}:1, under its {floor}:1 floor (base {base:.3} + wedge {wedge:.3} + right {rw:.3})"
                );
                assert!(after > before, "{label} over art {art}: the wedge made it WORSE ({before:.2} → {after:.2})");
            }
        }
    }

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

    // ---- the travelling capsules --------------------------------------------------------------
    // `Spring::step` is `gfx::spring`, pure and already driven frame-by-frame by `card_row.rs`'s
    // tests, so capsule MOTION is fully host-testable. What is not: anything through
    // `with_tab_metrics` (it measures with SDL2_ttf), which is why these drive the pure
    // `Capsule`/`TabStrip` against `widths_for` spans instead of the real row.

    /// One frame at the app's own cadence, the value `card_row.rs`'s tests step with.
    const DT: f32 = 1.0 / 60.0;
    /// Pill `i`'s content-space `(x, w)` in the fixture strip — the same pair `tab_row_update` hands
    /// the strip, built from the same two functions.
    fn span_of(w: &[f32], i: usize) -> (f32, f32) {
        (tab_pill_x(w, i), w[i])
    }

    /// The one rule both the fill and the ink read, at every edge it has.
    #[test]
    fn cap_cover_is_the_pills_own_share_of_what_is_over_it() {
        let pill = (100.0, 200.0);
        assert_eq!(cap_cover(pill, pill), 1.0, "a capsule sitting exactly on the pill covers it");
        assert_eq!(cap_cover(pill, (0.0, 100.0)), 0.0, "abutting on the left is not covering");
        assert_eq!(cap_cover(pill, (300.0, 100.0)), 0.0, "…nor on the right");
        assert_eq!(cap_cover(pill, (0.0, 200.0)), 0.5, "half over the left edge is half the pill");
        assert_eq!(cap_cover(pill, (200.0, 400.0)), 0.5, "…and half over the right edge likewise");
        assert_eq!(cap_cover(pill, (-500.0, 5000.0)), 1.0, "a capsule wider than the pill still covers 1, not more");
        let z = cap_cover((100.0, 0.0), pill);
        assert_eq!(z, 0.0, "a zero-width pill covers nothing");
        assert!(z.is_finite(), "…and does not divide by zero into a NaN that would poison every ink");
    }

    /// The regression that would otherwise draw a bright capsule sweeping the whole strip on the
    /// first Library frame: an UNPLACED capsule lands on its pill, it does not fly to it. Alpha still
    /// fades in, because arriving in a row is a fade — the two are deliberately different motions.
    #[test]
    fn a_capsule_lands_on_its_first_pill_instead_of_flying_in_from_the_origin() {
        let w = widths_for(12);
        let p7 = span_of(&w, 7);
        let mut c = Capsule::new();
        c.step(Some((7, p7)), DT);
        assert_eq!(cap_cover(p7, c.span()), 1.0, "the first frame must already be ON pill 7");
        assert!(c.alpha() > 0.0 && c.alpha() < 1.0, "…while its alpha is still ramping up ({})", c.alpha());
    }

    /// A capsule mid-travel must always be sitting on SOMETHING. The label ink is derived from
    /// coverage, so a frame where neither neighbour is covered is a frame where both labels read as
    /// plain while a bright capsule floats in the gutter between them.
    #[test]
    fn a_travelling_capsule_is_never_sitting_on_nothing() {
        let w = widths_for(12);
        let (p0, p1) = (span_of(&w, 0), span_of(&w, 1));
        let mut c = Capsule::new();
        for _ in 0..60 {
            c.step(Some((0, p0)), DT);
        }
        assert!(cap_cover(p0, c.span()) > 0.99, "the fixture must start settled on pill 0");

        c.step(Some((1, p1)), DT);
        assert!(cap_cover(p0, c.span()) > 0.5, "one frame in it must still be mostly on pill 0, not teleported");
        for f in 0..60 {
            c.step(Some((1, p1)), DT);
            let on = cap_cover(p0, c.span()).max(cap_cover(p1, c.span()));
            assert!(on > 0.0, "frame {f}: the capsule is on neither pill (span {:?})", c.span());
        }
        assert!(cap_cover(p1, c.span()) > 0.99, "it must arrive fully on pill 1");
        assert!(cap_cover(p0, c.span()) < 0.01, "…and leave pill 0 behind");
    }

    /// Focus leaving the row fades the capsule where it stands rather than parking it at the origin,
    /// and coming back LANDS — the "capsule streaks across the row when you come back from the grid"
    /// failure, which is the whole reason [`CAP_LAND_A`] exists.
    #[test]
    fn focus_leaving_the_row_fades_in_place_and_coming_back_lands() {
        let w = widths_for(12);
        let (p2, p9) = (span_of(&w, 2), span_of(&w, 9));
        let mut c = Capsule::new();
        for _ in 0..60 {
            c.step(Some((2, p2)), DT);
        }
        let held = c.span();
        for _ in 0..40 {
            c.step(None, DT);
        }
        assert!(c.alpha() < CAP_LAND_A, "focus off the row must fade the capsule out ({})", c.alpha());
        assert_eq!(c.span(), held, "…in place: an invisible capsule must not also drift");

        c.step(Some((9, p9)), DT);
        assert_eq!(cap_cover(p9, c.span()), 1.0, "returning focus to a FAR pill lands on it, never glides");
    }

    /// The two-capsule model, locked against a regression to one: on the Library screen the selected
    /// tab is the section you are browsing while focus walks the row, so a pill can be wearing either,
    /// both, or neither — and the mixes it is inked with are exactly that answer.
    #[test]
    fn the_ink_a_pill_wears_is_exactly_the_capsule_over_it() {
        let w = widths_for(12);
        let settle = |sel: c_int, foc: c_int| {
            let mut s = TabStrip::new();
            for _ in 0..90 {
                s.update(sel, foc, |i| (i < w.len()).then(|| span_of(&w, i)), DT);
            }
            s
        };
        let s = settle(2, 5);
        let near = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
        assert!(near(s.mixes(span_of(&w, 2)), (0.0, 1.0)), "the selected pill wears only the selection");
        assert!(near(s.mixes(span_of(&w, 5)), (1.0, 0.0)), "the focused pill wears only the focus");
        assert!(near(s.mixes(span_of(&w, 0)), (0.0, 0.0)), "an idle pill wears neither");
        let both = settle(3, 3);
        assert!(near(both.mixes(span_of(&w, 3)), (1.0, 1.0)), "one pill can wear both at once");
        // …and nothing is marked at all when there is nothing to mark (an emptied season strip, or
        // focus off the row on a page that has no selection either).
        let none = settle(-1, -1);
        assert!(near(none.mixes(span_of(&w, 0)), (0.0, 0.0)), "no target means no ink anywhere");
    }

    /// Ink CONSERVATION across a travel: the two capsules only ever have one pill's worth of coverage
    /// between them, so no frame can ink two labels toward `ACCENT_INK` at once. This is what bounds
    /// the one transient the design accepts — a partly covered pill inks its WHOLE label — to half a
    /// label on each of two pills for the length of one travel, rather than a whole row dimming.
    #[test]
    fn a_travel_never_inks_more_than_one_pills_worth_of_label() {
        let w = widths_for(12);
        let mut s = TabStrip::new();
        let span = |i: usize| (i < w.len()).then(|| span_of(&w, i));
        for _ in 0..90 {
            s.update(-1, 4, span, DT);
        }
        for f in 0..90 {
            s.update(-1, 5, span, DT);
            let total: f32 = (0..w.len()).map(|i| s.mixes(span_of(&w, i)).0).sum();
            assert!(total <= 1.0 + 1e-3, "frame {f}: {total} pills' worth of focus ink is lit at once");
            assert!(total > 0.0, "frame {f}: the focus ink went out entirely mid-travel");
        }
    }

    /// Every RESTING state a strip-driven pill can be in must be the look it replaced, to the bit —
    /// this is the whole reason the mix lerps between the same three ink roles the boolean arms pick
    /// from rather than inventing a ramp of its own.
    #[test]
    fn a_settled_mixed_pill_is_the_boolean_look_it_replaced() {
        // "to the bit" means to the PANEL's bit: a lerp that lands on its endpoint still carries a
        // ~3e-8 float residue, and the framebuffer is 8 bits per channel.
        let is = |got: [f32; 4], want: [f32; 4], what: &str| {
            for i in 0..4 {
                assert!(
                    (got[i] - want[i]).abs() < 0.5 / 255.0,
                    "{what}: channel {i} is {} not {} — over half a display code out",
                    got[i],
                    want[i]
                );
            }
        };
        is(TabPill::mixed_ink(0.0, 0.0), theme::TEXT_TERTIARY, "plain segment");
        is(TabPill::mixed_ink(0.0, 1.0), theme::TEXT_PRIMARY, "selected, focus elsewhere");
        is(TabPill::mixed_ink(1.0, 0.0), crate::ui::ACCENT_INK, "focused");
        is(TabPill::mixed_ink(1.0, 1.0), crate::ui::ACCENT_INK, "focus outranks selection");
        // out-of-range mixes clamp rather than extrapolating past the tokens
        is(TabPill::mixed_ink(-1.0, 4.0), theme::TEXT_PRIMARY, "an over-range selection clamps");
        is(TabPill::mixed_ink(9.0, -9.0), crate::ui::ACCENT_INK, "an over-range focus clamps");
    }

    /// **The plated composite**, which is arithmetic and therefore has no business being graded by
    /// eye. A season tab's ground is now TWO layers — the pill's own idle plate and the strip's
    /// travelling selection capsule over it — where it used to be one flat token. Same-colour
    /// source-over is `a + b − ab`, so the pair must land back on [`theme::TAB_PLATE_SELECTED`]; and
    /// the plate must retire exactly as the OPAQUE focus capsule arrives, or a focused season would
    /// wear a 0.08 white film its unplated twin in the top row does not.
    #[test]
    fn a_travelling_plate_lands_back_on_the_plate_it_replaced() {
        let idle = theme::TAB_PLATE_IDLE[3];
        let over = theme::TAB_PLATE_SELECTED_OVER[3];
        let composite = over + idle * (1.0 - over);
        assert!(
            (composite - theme::TAB_PLATE_SELECTED[3]).abs() < 1.0 / 255.0,
            "capsule .{over} over plate .{idle} composites to {composite}, not TAB_PLATE_SELECTED's {}",
            theme::TAB_PLATE_SELECTED[3]
        );
        // order-independence is what lets the pill keep painting its plate AFTER the strip drew the
        // capsule under it — the draw order this component actually uses
        assert!(
            ((idle + over * (1.0 - idle)) - composite).abs() < 1e-6,
            "same-colour source-over must be order-independent, or the draw order becomes load-bearing"
        );
        // all three plate tokens are the same white; only their weight differs
        for i in 0..3 {
            assert_eq!(theme::TAB_PLATE_SELECTED_OVER[i], theme::TAB_PLATE_IDLE[i], "channel {i}");
            assert_eq!(theme::TAB_PLATE_SELECTED_OVER[i], theme::TAB_PLATE_SELECTED[i], "channel {i}");
        }
        // …and the split survives the `Painter` cascade, which scales BOTH layers: two layers under
        // a cascade are not the same function as one, so the drift is bounded here rather than
        // assumed. (Nothing draws a tab strip at α<1 today; this is the guard for the day one does.)
        for &a in &[1.0f32, 0.75, 0.5, 0.25] {
            let one = a * theme::TAB_PLATE_SELECTED[3];
            let two = a * over + a * idle * (1.0 - a * over);
            assert!(
                (one - two).abs() < 1.0 / 255.0,
                "at cascade α={a} the two-layer plate is {two} against the old {one} — over a display code apart"
            );
        }
    }
}
