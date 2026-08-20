//! Vector icon assets (SVG) rasterized at runtime into tinted GL textures — the iOS-style
//! "ship the vector, render at runtime" approach. Each icon lives as an SVG file under
//! assets/icons/ (authored as a white #ffffff mask), embedded via include_str!. On first use
//! at a given pixel size we rasterize it (crate::svg → nanosvg), upload it once as a GL texture
//! (cached), and draw it through the Painter with a per-state tint. Main/GL-thread only.
//!
//! ## Authoring contract (what an asset may contain)
//!
//! The result is a **mask**: only alpha survives, so gradients and multi-colour fills are wasted
//! and the tint is the whole colour story. Beyond that, two rules that are not obvious until a
//! mark looks wrong on the panel — both verified by rasterizing through `src/svg.c` itself:
//!
//! 1. **A mark is ONE `<path>`; a composite mark is that path's overlapping SUBPATHS.** Subpaths
//!    of one path are winding-unioned by the rasterizer, so the joins carry no seam. Separate
//!    `<circle>`/`<path>` ELEMENTS are alpha-composited instead — `a1 + a2(1-a1)` — so wherever
//!    two antialiased edges run together the union lands at ~0.75 alpha and the mark wears a
//!    visible crease. The pre-redraw `popcorn-spilled.svg` did exactly that (140/255 at 34px,
//!    16/255 at 136px — a composite seam gets WORSE with resolution, which is how it tells itself
//!    apart from a real notch).
//! 2. **Every subpath winds the same way** (these are all clockwise). Nonzero fill turns a
//!    counter-clockwise subpath into a HOLE punched through whatever it overlaps, which looks
//!    like a rasterizer bug and is not one.
//!
//! Grade a new mark by rasterizing it at its real draw size and at 4×: full opacity reached, no
//! sub-255 pixel more than 2px inside the ink except where the geometry really is notched (it
//! resolves to a clean gap at 4×), and no ink on the border.
#![allow(dead_code)]
use crate::gfx::upload_rgba;
use crate::ui::{Painter, Rect};
use std::os::raw::c_uint;
use std::ptr::addr_of_mut;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Cc,
    Audio,
    Check,
    Chevron, // points RIGHT; the directional variants below are separate masks (the
    // rasterizer draws untransformed, so direction is per-asset, not a rotation)
    ChevronDown,
    ChevronUp,
    /// The watch-state ACTIONS, as **filled** discs: a check knocked out of one for "Mark as
    /// Watched", a minus knocked out of one for "Mark as Unwatched" (`item_menu::state_rows`).
    /// Filled is the rule, not a preference: it is what stops an ACTION being read as a STATE. The
    /// leading column carries a picker's bare tick or an action's glyph and nothing else — a switch
    /// states itself as a word at the row's trailing edge (`Row::toggle`), so a hollow circle here
    /// would be a third grammar for the same column. (There was one, briefly, on 2026-08-13: a
    /// ring/ticked-ring pair. The design system deleted both assets the same evening.)
    ///
    /// Both are one `<path>` with **`fill-rule="evenodd"`**, which is how the mark is knocked out of
    /// the disc — the one place this set departs from the all-subpaths-wind-the-same-way rule above,
    /// and it is load-bearing: under nonzero the knockout fills solid and the mark disappears.
    /// Verified through `src/svg.c` itself at 26px and 4×, where the gap resolves clean.
    CheckCircleFill,
    /// See [`Icon::CheckCircleFill`].
    MinusCircleFill,
    /// A bare horizontal stroke — the "remove" half of the bare [`Icon::Check`], for a control that
    /// is ALREADY a circle (a hero or detail disc button), where a filled disc inside a disc would
    /// be two circles saying one thing. Its user is the detail hero's *mark unwatched* disc, beside
    /// a `Check` on the same ground: the pair `detail::hero_ctls` draws for a part-watched item.
    Minus,
    Play,
    Pause,
    /// Counter-clockwise circular arrow (↺) — "play from the start", the detail hero's restart disc.
    Restart,
    Info,
    /// Warning triangle — `info.svg`'s sibling (same 24 viewBox, 2.2 stroke, round caps/joins,
    /// dot-and-bar inverted). From `Plex Pass Awareness.dc.html`: the facts row's HDR chip at
    /// ~24px and the playback-failed read-out at 96px. Outline, not filled — a solid triangle
    /// reads as an error state where this marks a warning or a verdict already worded in text.
    Alert,
    User,
    Backspace,
    /// A screen with a play triangle — the item menu's "Go to Episode" leading glyph.
    Episode,
    /// A stack of layers — the item menu's "Go to Show" leading glyph (a series of episodes).
    Show,
    /// A play triangle behind a leading bar — "Play from Start" (restart, not resume).
    PlayStart,
    /// An X — "remove this" (the item menu's Remove from Continue Watching row).
    Close,
    /// A horizontal ellipsis — "more options". The player transport's third control disc, which
    /// opens the overflow popover (`ui/more_menu.rs`). Overflow, so it sits at the END of the row.
    More,
    /// The magnifier — the only pill in the shared top strip that is a MARK instead of a word
    /// (`Search Screen.dc.html`). Drawn at 1.15× the strip's own type rung, inked exactly as a
    /// label would be, so it reads as one of the row rather than as an ornament on it.
    ///
    /// ONE `<path>`, two subpaths (the ring as a pair of half-arcs, then the handle), both STROKED
    /// — `info.svg`'s construction, and the reason it is one element rather than a `<circle>` plus
    /// a `<line>`: separate elements alpha-composite, and where the handle meets the ring their two
    /// antialiased edges would land at ~0.75 and wear a visible crease (see the module doc). The
    /// handle also starts just outside the ring, so the round caps close the joint without the two
    /// strokes overlapping at all.
    Search,
    // ---- review-score marks (the detail hero's ratings row) ----
    //
    // These are OUR OWN drawings, not reproductions. Rotten Tomatoes' marks — the fruit, the
    // Certified Fresh seal, the popcorn tub — were shipped here until 2026-08-02 and removed:
    // there is no licensing route for them (RT's developer programme is closed to unofficial
    // projects and `developer.fandango.com` does not resolve), and redrawing a mark is the
    // standard infringement pattern rather than a defence. The provider is now NAMED in text
    // instead, which is referential use and needs no licence — so the glyph no longer has to say
    // *whose* score this is. It only has to carry the VERDICT, which is what these four do.
    //
    // Two layers per mark for the same reason as before: the rasterizer renders a MASK and the
    // colour is the tint (`theme::RATING_*`), so a two-tone mark needs two masks at one rect.
    // Both critic layers share one 26×26 viewBox and register exactly.
    /// The tomato's body. Tinted [`theme::RATING_FRESH`] for a ripe score and
    /// [`theme::RATING_CERTIFIED`] for the rarer Certified bar — the SAME fruit struck in gold,
    /// not a seal, which is the one substantive simplification against the retired art.
    Tomato,
    /// The stem-and-sepals over [`Icon::Tomato`]. Painted [`theme::RATING_LEAF`] on a fresh or
    /// certified body and [`theme::RATING_MUTED`] on a hollow one. Its base sits INSIDE the body's
    /// silhouette, so the two masks overlap solidly instead of meeting at a seam.
    TomatoCalyx,
    /// A rotten score: the same fruit drained to an outline. A stroked ring rather than a splat —
    /// the negation is "the colour has gone out of it", which needs no second device.
    TomatoHollow,
    /// The audience mark: a CROWD — two figures under one shoulder line, so it reads as "many
    /// people" rather than "a person". One layer, and the only mark here with four elements in it:
    /// permitted because none of them touch (heads clear their bodies by ~2.5 units and the two
    /// figures do not overlap in x), so there is no antialiased seam for them to composite across.
    /// It negates by DRAINING to [`theme::RATING_MUTED`] rather than going hollow: a single fruit
    /// can carry an outline, but outlining every shape in a crowd is a tangle of strokes at 30px.
    Crowd,
}

fn src(id: Icon) -> &'static str {
    match id {
        Icon::Cc => include_str!("../../../assets/icons/cc.svg"),
        Icon::Audio => include_str!("../../../assets/icons/audio.svg"),
        Icon::Check => include_str!("../../../assets/icons/check.svg"),
        Icon::Chevron => include_str!("../../../assets/icons/chevron.svg"),
        Icon::ChevronDown => include_str!("../../../assets/icons/chevron-down.svg"),
        Icon::ChevronUp => include_str!("../../../assets/icons/chevron-up.svg"),
        Icon::CheckCircleFill => include_str!("../../../assets/icons/check-circle-fill.svg"),
        Icon::MinusCircleFill => include_str!("../../../assets/icons/minus-circle-fill.svg"),
        Icon::Minus => include_str!("../../../assets/icons/minus.svg"),
        Icon::Play => include_str!("../../../assets/icons/play.svg"),
        Icon::Pause => include_str!("../../../assets/icons/pause.svg"),
        Icon::Restart => include_str!("../../../assets/icons/restart.svg"),
        Icon::Info => include_str!("../../../assets/icons/info.svg"),
        Icon::Alert => include_str!("../../../assets/icons/alert.svg"),
        Icon::User => include_str!("../../../assets/icons/user.svg"),
        Icon::Backspace => include_str!("../../../assets/icons/backspace.svg"),
        Icon::Episode => include_str!("../../../assets/icons/episode.svg"),
        Icon::Show => include_str!("../../../assets/icons/show.svg"),
        Icon::PlayStart => include_str!("../../../assets/icons/play-start.svg"),
        Icon::Close => include_str!("../../../assets/icons/close.svg"),
        Icon::More => include_str!("../../../assets/icons/more.svg"),
        Icon::Search => include_str!("../../../assets/icons/search.svg"),
        Icon::Tomato => include_str!("../../../assets/icons/tomato.svg"),
        Icon::TomatoCalyx => include_str!("../../../assets/icons/tomato-calyx.svg"),
        Icon::TomatoHollow => include_str!("../../../assets/icons/tomato-hollow.svg"),
        Icon::Crowd => include_str!("../../../assets/icons/crowd.svg"),
    }
}

// (icon, px) → GL texture. The UI is fixed 1080p so only a handful of (icon,size) pairs ever
// appear; a flat Vec is plenty. Rasterize+upload once, then reuse.
struct Entry {
    id: Icon,
    px: i32,
    tex: c_uint,
}
static mut CACHE: Vec<Entry> = Vec::new();

// Antialias the way text does — rasterise the vector at the *exact* draw size and keep nanosvg's own
// coverage-AA edge (SS = 1: no supersample). The old path rasterised SS× larger and let GL minify it
// down, but GL_LINEAR only samples 2×2 texels for an SS×SS footprint, so it under-filtered and
// re-aliased the edge (and GLES2 can't mipmap these NPOT masks). Drawing 1:1 keeps the edge crisp.
// `downsample_alpha` then just normalises rgb → white so straight-alpha edges never fringe dark.
// (Bump SS to supersample + box-downsample here if a size ever looks jaggy.)
const SS: i32 = 1;

fn tex_for(id: Icon, px: i32) -> c_uint {
    unsafe {
        let cache = &mut *addr_of_mut!(CACHE);
        if let Some(e) = cache.iter().find(|e| e.id == id && e.px == px) {
            return e.tex;
        }
        let target = px.clamp(8, 96);
        let hi = target * SS;
        let tex = match crate::svg::rasterize(src(id), hi, hi) {
            Some(rgba) => {
                let small = downsample_alpha(&rgba, hi, SS);
                upload_rgba(0, target, target, small.as_ptr())
            }
            None => 0,
        };
        cache.push(Entry { id, px, tex });
        tex
    }
}

/// Box-average each `ss`×`ss` block of the supersampled mask into one output texel. The alpha is the
/// mean coverage (the clean AA edge); rgb is forced white so bilinear/compositing never darkens the
/// edge — the icon's colour comes entirely from the draw tint (`FS_IMG`: `c.rgb*tint.rgb`, coverage
/// `c.a`), so straight-alpha edge pixels would otherwise fringe dark.
fn downsample_alpha(src: &[u8], sw: i32, ss: i32) -> Vec<u8> {
    let dw = sw / ss;
    let mut out = vec![255u8; (dw * dw * 4) as usize];
    let n = (ss * ss) as u32;
    for y in 0..dw {
        for x in 0..dw {
            let mut a = 0u32;
            for jy in 0..ss {
                for jx in 0..ss {
                    a += src[(((y * ss + jy) * sw + (x * ss + jx)) * 4 + 3) as usize] as u32;
                }
            }
            out[((y * dw + x) * 4 + 3) as usize] = (a / n) as u8;
        }
    }
    out
}

/// Draw icon `id` filling `r` (rasterized+cached at r's pixel size), tinted `tint` (a white mask
/// times the tint = a solid-colour icon; tint alpha fades it). No-op if rasterization failed.
pub(crate) fn draw(p: Painter, id: Icon, r: Rect, tint: [f32; 4]) {
    let px = r.w.max(r.h).round() as i32;
    if px <= 0 {
        return;
    }
    let tex = tex_for(id, px);
    if tex != 0 {
        // 1:1 mask — snap the COMPOSITED origin (fold the painter translate, snap, unfold),
        // same contract as text; see gfx::snap.
        let r = Rect::new(
            crate::gfx::snap(r.x + p.dx) - p.dx,
            crate::gfx::snap(r.y + p.dy) - p.dy,
            r.w,
            r.h,
        );
        p.tex(tex, r, 0.0, tint);
    }
}
