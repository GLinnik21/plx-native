//! Vector icon assets (SVG) rasterized at runtime into tinted GL textures — the iOS-style
//! "ship the vector, render at runtime" approach. Each icon lives as an SVG file under
//! assets/icons/ (authored as a white #ffffff mask), embedded via include_str!. On first use
//! at a given pixel size we rasterize it (crate::svg → nanosvg), upload it once as a GL texture
//! (cached), and draw it through the Painter with a per-state tint. Main/GL-thread only.
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
    Chevron,
    Play,
    Pause,
    Info,
    User,
    Backspace,
}

fn src(id: Icon) -> &'static str {
    match id {
        Icon::Cc => include_str!("../../../assets/icons/cc.svg"),
        Icon::Audio => include_str!("../../../assets/icons/audio.svg"),
        Icon::Check => include_str!("../../../assets/icons/check.svg"),
        Icon::Chevron => include_str!("../../../assets/icons/chevron.svg"),
        Icon::Play => include_str!("../../../assets/icons/play.svg"),
        Icon::Pause => include_str!("../../../assets/icons/pause.svg"),
        Icon::Info => include_str!("../../../assets/icons/info.svg"),
        Icon::User => include_str!("../../../assets/icons/user.svg"),
        Icon::Backspace => include_str!("../../../assets/icons/backspace.svg"),
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
