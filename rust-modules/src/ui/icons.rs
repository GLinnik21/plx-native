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

// Supersample: rasterize the mask at SS× the draw size and let GL's bilinear filter downsample it.
// A thin stroke rasterized 1:1 is mostly partial-coverage pixels (reads as faint/transparent);
// rendering it large first gives a solid, cleanly-anti-aliased edge when scaled down.
const SS: i32 = 3;

fn tex_for(id: Icon, px: i32) -> c_uint {
    unsafe {
        let cache = &mut *addr_of_mut!(CACHE);
        if let Some(e) = cache.iter().find(|e| e.id == id && e.px == px) {
            return e.tex;
        }
        let hi = (px * SS).clamp(24, 256);
        let tex = match crate::svg::rasterize(src(id), hi, hi) {
            Some(rgba) => upload_rgba(0, hi, hi, rgba.as_ptr()),
            None => 0,
        };
        cache.push(Entry { id, px, tex });
        tex
    }
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
        p.tex(tex, r, 0.0, tint);
    }
}
