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
    /// Hollow circle — the Unwatched toolbar chip's off state. It used to double as The Movie
    /// Database's mark; [`Icon::Tmdb`] is that now, so the chip is free to change shape without
    /// silently redrawing a brand mark.
    Ring,
    /// The amber unwatched corner mark (top-right of a poster): a right triangle whose outer
    /// corner is pre-rounded to sit flush inside the card's 14px corner radius. Filled mask.
    UnwatchedAngle,
    Play,
    Pause,
    /// Counter-clockwise circular arrow (↺) — "play from the start", the detail hero's restart disc.
    Restart,
    Info,
    User,
    Backspace,
    /// A screen with a play triangle — the item menu's "Go to Episode" leading glyph.
    Episode,
    /// A stack of layers — the item menu's "Go to Show" leading glyph (a series of episodes).
    Show,
    /// A check inside a circle — the item menu's watched-state ACTION (distinct from [`Icon::Check`],
    /// which marks the already-active row in a picker).
    CheckCircle,
    /// A play triangle behind a leading bar — "Play from Start" (restart, not resume).
    PlayStart,
    /// An X — "remove this" (the item menu's Remove from Continue Watching row).
    Close,
    // ---- review-score brand marks (the detail hero's ratings row). Which of the five Rotten
    // Tomatoes marks a badge draws comes from the server's `Rating.image` state, never from the
    // score — see `metadata::RatingArt`. Each asset is a plain silhouette because the rasterizer
    // renders a MASK and the brand colour is the tint (`theme::RATING_*`).
    //
    // The Rotten Tomatoes marks are drawn as TWO STACKED MASKS, not one tinted silhouette: a base
    // and an accent layer (`*-calyx`, `*-kernels`) at the same rect and size, so the tomato gets a
    // green leaf on a red body and the tub gets gold popcorn. `Details Screen.dc.html` draws them
    // multi-tone, and it is right to: at the 30px the row uses, a flat-red tomato has lost its leaf
    // and reads as a red blob, and a mono tub has lost its popcorn. Both layers rasterize from the
    // same 24×24 viewBox, so they register exactly — see `ui::detail::rating_mark`, which is the one
    // place that pairs them.
    //
    // Sized for the row's 30px mark (`detail::RATING_MARK_D`) rather than scaled down from a
    // poster-sized drawing — at that size the mark carries the VERDICT, so anything that blurs into
    // a blob has failed. ----
    /// Rotten Tomatoes' fresh tomato, red body — `rottentomatoes://image.rating.ripe`. Base layer of
    /// the pair; [`Icon::TomatoCalyx`] paints its leaf.
    Tomato,
    /// The stem-and-sepals of [`Icon::Tomato`], for painting green over it.
    TomatoCalyx,
    // Certified Fresh — `…image.rating.certified` — is RT's SEAL, not a variant of the fruit: a gold
    // disc carrying a red box, a green calyx and a green banner. Three layers, drawn in this order.
    /// The gold seal disc, back-most layer of Certified Fresh.
    TomatoCertifiedSeal,
    /// The red box on the seal — the middle layer.
    TomatoCertifiedBox,
    /// The green calyx and banner — the front layer.
    TomatoCertifiedGreen,
    /// Rotten Tomatoes' splattered tomato — `…image.rating.rotten`. The one RT mark with no accent
    /// layer: a splat is one substance, and a second hue on it reads as two splats.
    TomatoRotten,
    /// The upright popcorn tub (audience score) — `…image.rating.upright`. Base layer;
    /// [`Icon::PopcornKernels`] paints its contents.
    Popcorn,
    /// The six crowning kernels of [`Icon::Popcorn`], for painting gold over it.
    PopcornKernels,
    /// The tipped, spilled tub — `…image.rating.spilled`. Base layer;
    /// [`Icon::PopcornSpilledKernels`] paints the thrown contents.
    PopcornSpilled,
    /// The three thrown kernels of [`Icon::PopcornSpilled`], for painting gold over it.
    PopcornSpilledKernels,
    /// Five-point star — the old IMDb mark. **Retired from the ratings row**, which now spells the
    /// brand with [`crate::ui::widgets::wordmark_chip`] instead (a generic star names no provider,
    /// where a gold `IMDb` chip is unmistakable). Kept because it is a good star and the row is one
    /// call away from wanting one back.
    Star,
    /// The Movie Database's score dial, a filled annulus. **Retired from the ratings row** for the
    /// same reason as [`Icon::Star`] — TMDB's brand IS a wordmark, and the chip can now spell it.
    /// Its own asset rather than [`Icon::Ring`]'s, so the Unwatched chip and a brand mark stop
    /// sharing one file.
    Tmdb,
}

fn src(id: Icon) -> &'static str {
    match id {
        Icon::Cc => include_str!("../../../assets/icons/cc.svg"),
        Icon::Audio => include_str!("../../../assets/icons/audio.svg"),
        Icon::Check => include_str!("../../../assets/icons/check.svg"),
        Icon::Chevron => include_str!("../../../assets/icons/chevron.svg"),
        Icon::ChevronDown => include_str!("../../../assets/icons/chevron-down.svg"),
        Icon::ChevronUp => include_str!("../../../assets/icons/chevron-up.svg"),
        Icon::Ring => include_str!("../../../assets/icons/ring.svg"),
        Icon::UnwatchedAngle => include_str!("../../../assets/icons/angle.svg"),
        Icon::Play => include_str!("../../../assets/icons/play.svg"),
        Icon::Pause => include_str!("../../../assets/icons/pause.svg"),
        Icon::Restart => include_str!("../../../assets/icons/restart.svg"),
        Icon::Info => include_str!("../../../assets/icons/info.svg"),
        Icon::User => include_str!("../../../assets/icons/user.svg"),
        Icon::Backspace => include_str!("../../../assets/icons/backspace.svg"),
        Icon::Episode => include_str!("../../../assets/icons/episode.svg"),
        Icon::Show => include_str!("../../../assets/icons/show.svg"),
        Icon::CheckCircle => include_str!("../../../assets/icons/check-circle.svg"),
        Icon::PlayStart => include_str!("../../../assets/icons/play-start.svg"),
        Icon::Close => include_str!("../../../assets/icons/close.svg"),
        Icon::Tomato => include_str!("../../../assets/icons/tomato.svg"),
        Icon::TomatoCalyx => include_str!("../../../assets/icons/tomato-calyx.svg"),
        Icon::TomatoCertifiedSeal => include_str!("../../../assets/icons/tomato-certified-seal.svg"),
        Icon::TomatoCertifiedBox => include_str!("../../../assets/icons/tomato-certified-box.svg"),
        Icon::TomatoCertifiedGreen => include_str!("../../../assets/icons/tomato-certified-green.svg"),
        Icon::TomatoRotten => include_str!("../../../assets/icons/tomato-rotten.svg"),
        Icon::Popcorn => include_str!("../../../assets/icons/popcorn.svg"),
        Icon::PopcornKernels => include_str!("../../../assets/icons/popcorn-kernels.svg"),
        Icon::PopcornSpilled => include_str!("../../../assets/icons/popcorn-spilled.svg"),
        Icon::PopcornSpilledKernels => include_str!("../../../assets/icons/popcorn-spilled-kernels.svg"),
        Icon::Star => include_str!("../../../assets/icons/star.svg"),
        Icon::Tmdb => include_str!("../../../assets/icons/tmdb.svg"),
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
