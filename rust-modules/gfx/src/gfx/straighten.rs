//! **The straighten pass: the frame's premultiplied colour, made straight for a compositor that
//! multiplies it by alpha** (#1328's follow-up).
//!
//! # The fact
//!
//! Over the hardware video plane the television composites our GLES surface as STRAIGHT alpha:
//! `panel = ui.rgb * ui.a + video * (1 - ui.a)`. Measured on the dev set (webOS 4.5, Mali-T820) from
//! the three plane captures of one translucent subtitle pixel: GRAPHIC `(57, 64, 70)` at `a` 0.275,
//! VIDEO `(226, 213, 51)`, DISPLAY `(182, 172, 55)`. Straight alpha predicts `(180, 172, 56)`;
//! premultiplied would be `(221, 218, 107)`. The multiply is in the SoC mixer, not in a surface
//! setting: the only compositor-side knob the firmware exports is `QWaylandQuickSurface::
//! setUseTextureAlpha`, and it does not reach the mixer.
//!
//! Our framebuffer, over the player's transparent clear, holds PREMULTIPLIED colour: the blend note in
//! `gfx.rs` (`glBlendFuncSeparate(SRC_ALPHA, ONE_MINUS_SRC_ALPHA, ONE, ONE_MINUS_SRC_ALPHA)`) leaves
//! `(rgb * a, a)` over `(0, 0, 0, 0)`. So every translucent pixel was multiplied by its own alpha a
//! second time on the panel: a pale subtitle glow, a soft glyph edge or a HUD scrim came out darker
//! than drawn, and a pale translucent kanji read as a dark shadow.
//!
//! # The pass
//!
//! A last full-viewport draw before the swap rewrites the frame `rgb' = a > 0 ? rgb / a : 0`, alpha
//! unchanged, blending off (`fs_straighten.frag`). The compositor's multiply then returns exactly the
//! colour the frame drew: `(rgb / a) * a + video * (1 - a) = rgb + video * (1 - a)`, the premultiplied
//! picture the frame was built as. An opaque pixel (`a = 1`) and a hole (`a = 0`) are fixed points.
//!
//! **Where it runs.** From the application loop (`app/run.rs`'s `present_and_swap`), after the frame's
//! draw and its one framebuffer readback and before anything else touches the surface, and only on a
//! frame whose surface is over the BOUND video plane (`Player::video_plane_bound`) that DREW something.
//! The gate presents every frame while the plane is bound, and about 99 % of those frames are a
//! transparent clear with no draw call at all; a full-viewport pass on each would make the whole film a
//! full-frame workload for nothing. Two reads of the frame's own state decide it: the plane bit
//! (a function argument, not gfx state: [`super::video_plane_frame`] is restored to `false` before
//! the draw returns, and it excludes the bound trailer preview, a `RenderStrategy::Page` screen whose
//! chrome is just as translucent) and [`note_draw`]'s count of draw calls since [`frame_begin`].
//!
//! **How it reads the frame.** Preferably in place: `GL_ARM_shader_framebuffer_fetch` (the dev set has
//! it, `docs/egl-partial-update-and-damage.md` §1b) or `GL_EXT_shader_framebuffer_fetch` read the
//! pixel the tile already holds, so the pass is one fragment program over tile memory: no copy, no
//! render-pass split. Without either, the frame is copied (`glCopyTexSubImage2D`, like
//! [`FrameCache::capture_prev`](super::FrameCache::capture_prev)) into one private viewport-sized
//! texture and drawn back 1:1 with blending off; that texture exists only on that path. It does NOT go
//! through [`FrameCache::capture`](super::FrameCache::capture): a capture arms the snapshot fence that
//! DEFERS presents until the copy leaves the GPU, which is right for a page snapshot and wrong for a
//! pass that runs on every frame with content for a whole playback. No new GL entry point is bound:
//! `glGetString`, `glCopyTexSubImage2D` and `glDrawArrays` were already here; the fetch is a GLSL
//! `#extension`, chosen from the extension string as a whole token and linked as an optional program.
//!
//! # Buffer age
//!
//! The surface is `EGL_BUFFER_DESTROYED` and the config has no `SWAP_BEHAVIOR_PRESERVED_BIT`
//! (`docs/egl-partial-update-and-damage.md` §1b), so a back buffer's contents are undefined after a
//! swap; and a frame on this plane starts from a clear (the player route's `glClear`, a preview page's
//! `frame_clear_through`) and redraws the whole viewport, because a presented frame has always been a
//! whole frame ("not dirty-RECTANGLE tracking", `app/run.rs`'s present gate). So the pass runs once,
//! over a buffer composed this frame from a clear, and nothing in a later frame is ever blended over
//! straight pixels. **A bound frame therefore has to stay a complete redraw.** A damage scheme
//! (`EGL_KHR_partial_update`, today only the `plxnative-egldamage` experiment) that redrew part of
//! it and blended over the pixels the buffer kept would composite premultiplied colour onto straight
//! colour: it must clear every damaged region first and run this pass over exactly that region.
//!
//! # Readbacks
//!
//! Every door that samples framebuffer 0 (`video_plane_refuses`) refuses once the pass has run
//! ([`straightened`]): what it would read is no longer a premultiplied picture. The simulator's own
//! readers (`shot`, the frame dump) run after its compositor model ([`super::draw_under`], which
//! composites the picture over a plane the way the television does, `UNDER`), never between the pass
//! and that model, so a screenshot is the panel and not the straight buffer.
//!
//! # Cost
//!
//! One full-viewport fragment program with a reciprocal and a multiply per pixel (about 2 arithmetic
//! words a quad on Midgard), over 2.07 M pixels: roughly half a millisecond of the Mali-T820's time by
//! the arithmetic counters' scale in `docs/egl-partial-update-and-damage.md` §2, UNMEASURED on the set.
//! The copy fallback adds the frame's copy and a pass reload (about 17 MB of traffic at 1080p). The
//! frame code keeps no damage rectangle to scissor the pass to, and a painted extent is not the logical
//! rect (`AA_BLEED`, shadow and glow pads), so it covers the viewport; the draw count is what keeps it
//! off the frames that have nothing on them.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint};
use std::sync::atomic::{AtomicU32, Ordering};

use super::{
    cap_tex, delete_tex, glActiveTexture, glBindFramebuffer, glBindTexture, glCopyTexSubImage2D, glDisable,
    glDrawArrays_raw, glEnable, glGetString, glTexParameteri, glViewport, link_optional_program, scissor_off,
    use_prog, FS_STRAIGHTEN_ARM, FS_STRAIGHTEN_COPY, FS_STRAIGHTEN_EXT, GL_BLEND, GL_FRAMEBUFFER,
    GL_NEAREST, GL_TEXTURE0, GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_TEXTURE_MIN_FILTER,
    GL_TRIANGLE_STRIP, VS_STRAIGHTEN,
};
use plx_base::eventlog::log;

// ---- the ledger of draw calls: "did this frame put anything on the surface?" --------------------

/// Draw calls issued since [`frame_begin`]. Every `glDrawArrays` in this crate goes through the
/// wrapper that calls [`note_draw`] (`gfx.rs` and `text.rs` each shadow the extern with one, and a
/// test pins that the raw import is called nowhere else), so a primitive cannot draw without being
/// counted. It counts draws to ANY target (a blur source's FBO, a region fill), never fewer than the
/// presented framebuffer's: the pass may run on a frame that did not need it, which is harmless (a
/// pixel with nothing on it is a fixed point), and can never be skipped on one that did.
static DRAWS: AtomicU32 = AtomicU32::new(0);

/// Book one draw call. The render thread is the only writer, so a load and a store is enough.
#[inline(always)]
pub(crate) fn note_draw() {
    DRAWS.store(DRAWS.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
}

/// Open a drawn frame: nothing has been drawn yet and the surface is not straightened. Called beside
/// `text::begin_frame`, once per presented frame, before the draw.
pub fn frame_begin() {
    DRAWS.store(0, Ordering::Relaxed);
    STRAIGHTENED.with(|s| s.set(false));
}

/// Has this frame drawn anything since [`frame_begin`]?
pub fn frame_drew() -> bool {
    DRAWS.load(Ordering::Relaxed) != 0
}

thread_local! {
    /// Set by the pass, cleared by [`frame_begin`]: the surface now holds STRAIGHT colour.
    ///
    /// THREAD-LOCAL like `VIDEO_PLANE_FRAME` beside the doors that read it, and for the same reason:
    /// it is draw state, the draw is one thread's, and a process-wide flag would let one test's
    /// straightened surface refuse another test's perfectly ordinary snapshot.
    static STRAIGHTENED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Has the pass run on this frame? Past it, framebuffer 0 holds straight colour, so a read of it (a
/// snapshot, a glass source, a field kick) would cache a picture the premultiplied blend cannot
/// composite: `video_plane_refuses` answers `true` for every door once this is.
pub fn straightened() -> bool {
    STRAIGHTENED.with(|s| s.get())
}

// ---- what to do: a pure decision ------------------------------------------------------------------

/// The framebuffer-fetch extension a driver offers, and the program variant that reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fetch {
    /// `GL_ARM_shader_framebuffer_fetch`: `gl_LastFragColorARM`. The Mali blob on the dev set.
    Arm,
    /// `GL_EXT_shader_framebuffer_fetch`: `gl_LastFragData[0]`. PowerVR and others.
    Ext,
}

/// Why the pass did not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// The surface is not over the bound video plane: it is opaque, and a pass would be an identity.
    NoPlane,
    /// Nothing was drawn since the clear: the frame is transparent black, a fixed point.
    NothingDrawn,
    /// Neither program linked (logged once at init); the frame goes out premultiplied, as before.
    NoProgram,
}

/// What the pass does this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Not at all.
    Skip(Skip),
    /// In place, reading the tile through this extension.
    Fetch(Fetch),
    /// Through a copy of the frame.
    Copy,
}

/// **The pass's schedule**: it runs on a frame over the BOUND plane that DREW something, in place when
/// the driver can fetch the framebuffer and through a copy when it cannot. `fetch` is the extension the
/// fetch program was linked for (`None` when it was not, for any reason) and `copy` says the copy
/// program linked.
pub fn plan(plane_bound: bool, drew: bool, fetch: Option<Fetch>, copy: bool) -> Plan {
    if !plane_bound {
        return Plan::Skip(Skip::NoPlane);
    }
    if !drew {
        return Plan::Skip(Skip::NothingDrawn);
    }
    match (fetch, copy) {
        (Some(f), _) => Plan::Fetch(f),
        (None, true) => Plan::Copy,
        (None, false) => Plan::Skip(Skip::NoProgram),
    }
}

/// Is `name` one of the space-separated entries of a `GL_EXTENSIONS` string? A WHOLE token, never a
/// prefix: `GL_ARM_shader_framebuffer_fetch` is the first half of `GL_ARM_shader_framebuffer_fetch_
/// depth_stencil`, which does not declare `gl_LastFragColorARM`.
fn extension_listed(list: &str, name: &str) -> bool {
    list.split_ascii_whitespace().any(|e| e == name)
}

/// The fetch extension a `GL_EXTENSIONS` string offers: ARM's when both are listed.
fn pick_fetch(list: &str) -> Option<Fetch> {
    if extension_listed(list, "GL_ARM_shader_framebuffer_fetch") {
        Some(Fetch::Arm)
    } else if extension_listed(list, "GL_EXT_shader_framebuffer_fetch") {
        Some(Fetch::Ext)
    } else {
        None
    }
}

// ---- the programs and the copy texture ----------------------------------------------------------

/// The fetch program, 0 when none linked, and the extension it reads (0 none, 1 ARM, 2 EXT).
static FETCH_PROG: AtomicU32 = AtomicU32::new(0);
static FETCH_KIND: AtomicU32 = AtomicU32::new(0);
/// The copy program, 0 when not linked (it is linked only when there is no fetch program).
static COPY_PROG: AtomicU32 = AtomicU32::new(0);
/// The copy fallback's texture and the viewport size it holds; 0 until the first copy.
static COPY_TEX: AtomicU32 = AtomicU32::new(0);
static COPY_W: AtomicU32 = AtomicU32::new(0);
static COPY_H: AtomicU32 = AtomicU32::new(0);

const GL_EXTENSIONS: c_uint = 0x1F03;

fn fetch_kind() -> Option<Fetch> {
    match FETCH_KIND.load(Ordering::Relaxed) {
        1 => Some(Fetch::Arm),
        2 => Some(Fetch::Ext),
        _ => None,
    }
}

/// The driver's extension string, empty when it cannot be read. A desktop core profile (the
/// simulator) answers `glGetString(GL_EXTENSIONS)` with `GL_INVALID_ENUM`, an error that would sit in
/// the queue for the first `glGetError` a cache probes, so a context that is not ES is not asked.
fn gl_extensions() -> String {
    if !super::gl_is_es() {
        return String::new();
    }
    // SAFETY: render thread, current context; the string is NUL-terminated and driver-owned.
    unsafe {
        let p = glGetString(GL_EXTENSIONS);
        if p.is_null() {
            return String::new();
        }
        CStr::from_ptr(p as *const c_char).to_string_lossy().into_owned()
    }
}

/// Link the pass's program for this driver, once per GL context. Called by `init_gl`; a failure is
/// logged and leaves the pass off (or on its copy program), never a boot failure: a frame that goes
/// out premultiplied is the picture this app has always shown.
pub fn init() {
    COPY_TEX.store(0, Ordering::Relaxed);
    COPY_W.store(0, Ordering::Relaxed);
    COPY_H.store(0, Ordering::Relaxed);
    let (mut fetch_prog, mut kind) = (0, 0);
    if let Some(f) = pick_fetch(&gl_extensions()) {
        let fs = match f {
            Fetch::Arm => FS_STRAIGHTEN_ARM,
            Fetch::Ext => FS_STRAIGHTEN_EXT,
        };
        match link_optional_program(VS_STRAIGHTEN.as_ptr(), fs.as_ptr()) {
            Some(p) => {
                fetch_prog = p;
                kind = if f == Fetch::Arm { 1 } else { 2 };
            }
            None => log(&format!("straighten: the {f:?} framebuffer-fetch program failed to build - copying instead")),
        }
    }
    let copy_prog = if fetch_prog != 0 {
        0
    } else {
        link_optional_program(VS_STRAIGHTEN.as_ptr(), FS_STRAIGHTEN_COPY.as_ptr()).unwrap_or(0)
    };
    FETCH_PROG.store(fetch_prog, Ordering::Relaxed);
    FETCH_KIND.store(kind, Ordering::Relaxed);
    COPY_PROG.store(copy_prog, Ordering::Relaxed);
    log(&format!(
        "straighten: {}",
        match (fetch_kind(), copy_prog != 0) {
            (Some(Fetch::Arm), _) => "GL_ARM_shader_framebuffer_fetch - the pass reads the tile in place",
            (Some(Fetch::Ext), _) => "GL_EXT_shader_framebuffer_fetch - the pass reads the tile in place",
            (None, true) => "no framebuffer fetch - the pass copies the frame",
            (None, false) => "no program - frames over the video plane stay premultiplied",
        }
    ));
}

/// **The pass.** Turn this frame's premultiplied colour straight if the frame is over the bound plane
/// and drew something ([`plan`]); returns whether it ran. Call it LAST, after every draw and every
/// readback of the frame, before the swap; the surface is `straightened` from then on.
///
/// Restores nothing it does not own: the viewport and the presented framebuffer are rebound as the
/// frame's draw left them (the surface's own), the scissor is off (so is `LIVE_SCISSOR`) and the
/// blend is back on with the func the frame used.
pub fn straighten_surface(plane_bound: bool) -> bool {
    let plan = plan(plane_bound, frame_drew(), fetch_kind(), COPY_PROG.load(Ordering::Relaxed) != 0);
    let (vx, vy, vw, vh) = plx_base::surface::viewport();
    if matches!(plan, Plan::Skip(_)) || vw <= 0 || vh <= 0 {
        return false;
    }
    // SAFETY: the render thread, in the frame's own context, after the draw; plain GL state.
    unsafe {
        glBindFramebuffer(GL_FRAMEBUFFER, plx_base::surface::default_fb());
        glViewport(vx, vy, vw, vh);
        scissor_off();
        glDisable(GL_BLEND);
        match plan {
            Plan::Fetch(_) => use_prog(FETCH_PROG.load(Ordering::Relaxed)),
            Plan::Copy => {
                let tex = copy_target(vw, vh);
                glActiveTexture(GL_TEXTURE0);
                glBindTexture(GL_TEXTURE_2D, tex);
                glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, vx, vy, vw, vh);
                use_prog(COPY_PROG.load(Ordering::Relaxed));
            }
            Plan::Skip(_) => unreachable!("a skipped plan returned above"),
        }
        glDrawArrays_raw(GL_TRIANGLE_STRIP, 0, 4);
        if plan == Plan::Copy {
            glBindTexture(GL_TEXTURE_2D, 0);
        }
        glEnable(GL_BLEND);
    }
    STRAIGHTENED.with(|s| s.set(true));
    true
}

/// The copy fallback's texture, sized to the viewport (made again when the viewport changed) and
/// sampled NEAREST: the pass reads it 1:1 and a filter must not touch a pixel's colour.
unsafe fn copy_target(vw: c_int, vh: c_int) -> c_uint {
    let (w, h) = (vw as u32, vh as u32);
    let mut tex = COPY_TEX.load(Ordering::Relaxed);
    if tex == 0 || COPY_W.load(Ordering::Relaxed) != w || COPY_H.load(Ordering::Relaxed) != h {
        delete_tex(tex);
        tex = cap_tex(vw, vh);
        glBindTexture(GL_TEXTURE_2D, tex);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
        COPY_TEX.store(tex, Ordering::Relaxed);
        COPY_W.store(w, Ordering::Relaxed);
        COPY_H.store(h, Ordering::Relaxed);
    }
    tex
}

// ---- the television's compositor, as arithmetic --------------------------------------------------

/// One GL blend factor the simulator's compositor model uses: the data the GL call
/// ([`UnderBlend::gl`]) and the host test's model ([`UnderBlend::apply`]) both read, so the test
/// checks the factors the simulator actually sets.
#[cfg(any(test, feature = "hostsim"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendFactor {
    One,
    DstAlpha,
    OneMinusDstAlpha,
}

#[cfg(any(test, feature = "hostsim"))]
impl BlendFactor {
    /// The `GLenum`.
    pub const fn gl(self) -> c_uint {
        match self {
            BlendFactor::One => 0x0001,
            BlendFactor::DstAlpha => 0x0304,
            BlendFactor::OneMinusDstAlpha => 0x0305,
        }
    }

    /// The factor's value for a destination of alpha `dst_a`.
    fn weight(self, dst_a: f32) -> f32 {
        match self {
            BlendFactor::One => 1.0,
            BlendFactor::DstAlpha => dst_a,
            BlendFactor::OneMinusDstAlpha => 1.0 - dst_a,
        }
    }
}

/// A `glBlendFuncSeparate` as `(source, destination)` factors for colour and for alpha.
#[cfg(any(test, feature = "hostsim"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnderBlend {
    pub rgb: (BlendFactor, BlendFactor),
    pub alpha: (BlendFactor, BlendFactor),
}

#[cfg(any(test, feature = "hostsim"))]
impl UnderBlend {
    /// The four `glBlendFuncSeparate` arguments.
    pub const fn gl(self) -> [c_uint; 4] {
        [self.rgb.0.gl(), self.rgb.1.gl(), self.alpha.0.gl(), self.alpha.1.gl()]
    }

    /// What the blend writes for a source `src` over a destination `dst` (`[r, g, b, a]`, 0..1):
    /// `src * f_src + dst * f_dst`, clamped, which is what the GL blend equation `FUNC_ADD` does.
    pub fn apply(self, src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
        let rgb = (self.rgb.0.weight(dst[3]), self.rgb.1.weight(dst[3]));
        let a = (self.alpha.0.weight(dst[3]), self.alpha.1.weight(dst[3]));
        let mut out = [0.0; 4];
        for i in 0..3 {
            out[i] = (src[i] * rgb.0 + dst[i] * rgb.1).clamp(0.0, 1.0);
        }
        out[3] = (src[3] * a.0 + dst[3] * a.1).clamp(0.0, 1.0);
        out
    }
}

/// **The simulator's model of the television's compositor**: the decoded picture drawn UNDER the
/// frame, `out.rgb = video * (1 - ui.a) + ui.rgb * ui.a`, the straight-alpha mix the mixer performs on
/// whatever the surface holds, and `out.a = 1` (the panel is opaque). It is the measured arithmetic,
/// not what the frame was meant to show: a premultiplied pixel it is given comes out multiplied by its
/// own alpha, which is exactly the fault the pass exists to remove, so the simulator shows the bug
/// without the pass and the fix with it.
#[cfg(any(test, feature = "hostsim"))]
pub const UNDER: UnderBlend = UnderBlend {
    rgb: (BlendFactor::OneMinusDstAlpha, BlendFactor::DstAlpha),
    alpha: (BlendFactor::OneMinusDstAlpha, BlendFactor::One),
};

// ---- where the frame drew: the boxes the pass covers -----------------------------------------------

/// What a draw call writes to the surface, as the draw site declares it (see [`Ledger`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Extent {
    /// The authored-pixel quad `(x, y, w, h)` the vertex shader places the unit quad over: the very
    /// numbers the draw's `*_RECT` uniform holds.
    Quad([f32; 4]),
    /// Whatever target is bound, edge to edge: a draw with no rect to place it, and the fallback for
    /// every extent that is not known. The pass then covers the whole viewport.
    Target,
}

impl Extent {
    /// The quad `(x, y, w, h)`.
    pub const fn quad(x: f32, y: f32, w: f32, h: f32) -> Self {
        Extent::Quad([x, y, w, h])
    }
}

/// Edges past this (in either direction) are clamped to it, so a box that means "everything" and a
/// union of two boxes can neither overflow an `i32` nor an `i64` area.
const FAR: i32 = 1 << 28;
/// The box that stands for the whole surface (an unknown extent): every viewport lies inside it.
const EVERYTHING: PxBox = [-FAR, -FAR, FAR, FAR];
/// Pixels of slack on every side of a quad's box, for the rounding between the CPU's idea of the
/// quad and the pixels the rasterizer lights: the vertex stage is fp32 and the rasterizer snaps to
/// sub-pixel positions, neither of which a whole-pixel `floor`/`ceil` accounts for.
const MARGIN: i32 = 1;
/// How many boxes the pass draws at most. A bound on work, never on content: past it the two boxes
/// that waste the fewest pixels together become their bounding box, so every drawn pixel stays covered.
const SLOTS: usize = 16;

use super::PxBox;

/// The window-pixel box (GL's bottom-left origin, far edges exclusive) that holds every pixel the
/// authored quad `q` can light, for a viewport at `(vx, vy)` and `s` window pixels per authored pixel.
/// `None` for a quad with no area, which draws nothing; [`EVERYTHING`] for one that is not finite.
fn quad_box(q: [f32; 4], (vx, vy): (i32, i32), s: f32) -> Option<PxBox> {
    let [x, y, w, h] = q;
    if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite() && s > 0.0 && s.is_finite()) {
        return Some(EVERYTHING);
    }
    if w == 0.0 || h == 0.0 {
        return None;
    }
    let (ax0, ax1) = (x.min(x + w), x.max(x + w));
    let (ay0, ay1) = (y.min(y + h), y.max(y + h));
    let lo = |v: f32| v.floor().clamp(-(FAR as f32), FAR as f32) as i32;
    let hi = |v: f32| v.ceil().clamp(-(FAR as f32), FAR as f32) as i32;
    // Rows count upward from the bottom of the target, authored y downward from the top.
    let (wy0, wy1) = (vy as f32 + (plx_base::surface::LOGICAL_H - ay1) * s, vy as f32 + (plx_base::surface::LOGICAL_H - ay0) * s);
    Some([
        lo(vx as f32 + ax0 * s) - MARGIN,
        lo(wy0) - MARGIN,
        hi(vx as f32 + ax1 * s) + MARGIN,
        hi(wy1) + MARGIN,
    ])
}

/// **The boxes of the surface a frame drew into**, pairwise disjoint (the contract the pass leans on).
#[derive(Clone, Copy)]
struct Ledger {
    #[allow(dead_code)] // the red stub books nothing; the next commit reads it
    boxes: [PxBox; SLOTS],
    n: usize,
}

impl Ledger {
    const fn new() -> Self {
        Ledger { boxes: [[0; 4]; SLOTS], n: 0 }
    }

    fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Book a box the frame drew into.
    fn add(&mut self, _b: PxBox) {}

    /// The boxes the pass draws over, cut to the viewport `view`.
    fn covering(&self, view: PxBox) -> Vec<PxBox> {
        vec![view]
    }
}

/// The pass's quads for `ledger` over the viewport `view`, as the `(x0, y0, x1, y1)` fractions of the
/// viewport the vertex shader's `u_box` takes.
fn quads(ledger: &Ledger, view: PxBox) -> Vec<[f32; 4]> {
    let (w, h) = ((view[2] - view[0]) as f32, (view[3] - view[1]) as f32);
    ledger
        .covering(view)
        .into_iter()
        .map(|b| [(b[0] - view[0]) as f32 / w, (b[1] - view[1]) as f32 / h, (b[2] - view[0]) as f32 / w, (b[3] - view[1]) as f32 / h])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The television's compositor in closed form: the measured relation, on 8-bit channels, as
    /// floats in codes (no rounding, so a test sees the model's own error and not a second one).
    fn tv_panel(ui: [u8; 4], video: [u8; 3]) -> [f32; 3] {
        let a = ui[3] as f32 / 255.0;
        [0, 1, 2].map(|i| ui[i] as f32 * a + video[i] as f32 * (1.0 - a))
    }

    /// What the frame MEANT the panel to show: its premultiplied colour over the picture.
    fn intended_panel(premult: [u8; 4], video: [u8; 3]) -> [f32; 3] {
        let a = premult[3] as f32 / 255.0;
        [0, 1, 2].map(|i| premult[i] as f32 + video[i] as f32 * (1.0 - a))
    }

    /// The pass in integer codes: `fs_straighten.frag`'s arithmetic, `1 / a` and one multiply in
    /// floats and the 8-bit write's round-to-nearest.
    fn straighten_px(px: [u8; 4]) -> [u8; 4] {
        let a = px[3] as f32 / 255.0;
        let k = if a > 0.0 { 1.0 / a } else { 0.0 };
        let c = |v: u8| ((v as f32 / 255.0 * k).clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8;
        [c(px[0]), c(px[1]), c(px[2]), px[3]]
    }

    fn unit(p: [u8; 4]) -> [f32; 4] {
        p.map(|v| v as f32 / 255.0)
    }

    /// The pixel the dev set's three plane captures were taken of: a pale translucent subtitle pixel,
    /// premultiplied, alpha 70/255, over the video `(226, 213, 51)`.
    const SUBTITLE: [u8; 4] = [57, 64, 70, 70];
    const VIDEO: [u8; 3] = [226, 213, 51];
    /// What the panel showed (DISPLAY capture).
    const DISPLAY: [f32; 3] = [182.0, 172.0, 55.0];

    fn within(got: [f32; 3], want: [f32; 3], codes: f32) -> bool {
        (0..3).all(|i| (got[i] - want[i]).abs() <= codes)
    }

    /// **The simulator's compositor does what the television's does.** Drawing the picture under a
    /// frame with `UNDER`'s factors, over the unconverted premultiplied pixel of the dev set's capture,
    /// gives the DISPLAY the television showed - not the premultiplied picture `(221, 218, 107)` the
    /// sim used to show for it (its old `(ONE_MINUS_DST_ALPHA, ONE)` factors).
    #[test]
    fn the_simulator_composites_a_frame_as_the_television_does() {
        let video = [VIDEO[0], VIDEO[1], VIDEO[2], 255].map(|v| v as f32 / 255.0);
        let panel = UNDER.apply(video, unit(SUBTITLE));
        let codes = [panel[0] * 255.0, panel[1] * 255.0, panel[2] * 255.0];
        assert!(within(codes, DISPLAY, 3.0), "the sim's panel {codes:?} against the captured DISPLAY {DISPLAY:?}");
        assert!(
            !within(codes, intended_panel(SUBTITLE, VIDEO), 25.0),
            "…which is far from the premultiplied picture the frame meant ({:?})",
            intended_panel(SUBTITLE, VIDEO)
        );
        assert_eq!(panel[3], 1.0, "the composited panel is opaque");
    }

    /// The factors are the closed form, not a look-alike: for every alpha and a spread of colours the
    /// blend `UNDER` names equals `ui.rgb * ui.a + video * (1 - ui.a)`.
    #[test]
    fn the_under_blend_is_the_measured_straight_alpha_mix() {
        for a in 0..=255u8 {
            for &c in &[0u8, 1, 57, 128, 200, 255] {
                for &v in &[0u8, 51, 226, 255] {
                    let ui = [c, c / 2, 255 - c, a];
                    let video = [v, 255 - v, v / 3];
                    let blend = UNDER.apply([video[0], video[1], video[2], 255].map(|x| x as f32 / 255.0), unit(ui));
                    let want = tv_panel(ui, video);
                    let got = [blend[0] * 255.0, blend[1] * 255.0, blend[2] * 255.0];
                    assert!(within(got, want, 0.01), "ui {ui:?} over {video:?}: {got:?} vs {want:?}");
                }
            }
        }
    }

    /// The `GLenum`s are the ones GL names; a typo in a constant would blend with another factor.
    #[test]
    fn the_under_blend_hands_gl_its_factors() {
        assert_eq!(UNDER.gl(), [0x0305, 0x0304, 0x0305, 0x0001], "ONE_MINUS_DST_ALPHA, DST_ALPHA, ONE_MINUS_DST_ALPHA, ONE");
    }

    /// **The pass fixes the capture.** Straightened first, the dev set's pixel composites to the
    /// premultiplied picture `(221, 218, 107)` the frame was drawn as, through the same compositor
    /// model that gives DISPLAY without it.
    #[test]
    fn the_pass_makes_the_captured_pixel_show_what_was_drawn() {
        let straight = straighten_px(SUBTITLE);
        assert_eq!(straight[3], SUBTITLE[3], "alpha is the compositor's and is not touched");
        let video = [VIDEO[0], VIDEO[1], VIDEO[2], 255].map(|v| v as f32 / 255.0);
        let panel = UNDER.apply(video, unit(straight));
        let codes = [panel[0] * 255.0, panel[1] * 255.0, panel[2] * 255.0];
        assert!(within(codes, [221.0, 218.0, 107.0], 1.5), "the panel with the pass: {codes:?}");
        assert!(within(codes, intended_panel(SUBTITLE, VIDEO), 1.0), "within a code of the picture the frame drew");
    }

    /// **For every premultiplied pixel the pass lands within a code of the intended panel**, over a
    /// bright, a dark and a mid picture: the round trip `rgb / a`, rounded to 8 bits, then multiplied
    /// by `a` by the compositor loses at most half a code (`0.5 * a`).
    #[test]
    fn the_pass_is_within_a_code_of_the_intended_panel_for_every_premultiplied_pixel() {
        let mut worst = 0.0f32;
        for a in 0..=255u8 {
            for c in 0..=a {
                for &v in &[0u8, 100, 255] {
                    let premult = [c, a - c, c / 2, a];
                    let video = [v, v, v];
                    let shown = tv_panel(straighten_px(premult), video);
                    let want = intended_panel(premult, video);
                    for i in 0..3 {
                        worst = worst.max((shown[i] - want[i]).abs());
                    }
                }
            }
        }
        assert!(worst <= 0.5 + 1e-3, "the worst error is {worst} codes");
    }

    /// An opaque pixel and a hole are fixed points, which is what lets the pass skip a frame with
    /// nothing on it and what leaves an opaque ground (the trailer preview's shelves) as it was.
    #[test]
    fn opaque_pixels_and_holes_are_fixed_points() {
        for c in 0..=255u8 {
            assert_eq!(straighten_px([c, 255 - c, c / 3, 255]), [c, 255 - c, c / 3, 255]);
        }
        assert_eq!(straighten_px([0, 0, 0, 0]), [0, 0, 0, 0]);
        assert_eq!(straighten_px([200, 10, 90, 0]), [0, 0, 0, 0], "no colour survives a zero alpha: it would be a/0");
    }

    /// A colour above its alpha (an additive rim: the premultiplied invariant broken on purpose)
    /// saturates at white-in-that-channel; it never wraps.
    #[test]
    fn a_colour_above_its_alpha_saturates() {
        assert_eq!(straighten_px([200, 50, 100, 100]), [255, 128, 255, 100]);
    }

    /// The simulator's compositor model and the pass are a pair: without the pass the sim shows the
    /// darkening for a translucent pixel, and with it the same pixel is the one the frame drew.
    #[test]
    fn without_the_pass_a_translucent_pixel_is_darker_by_its_alpha() {
        let a = SUBTITLE[3] as f32 / 255.0;
        let shown = tv_panel(SUBTITLE, VIDEO);
        let meant = intended_panel(SUBTITLE, VIDEO);
        assert!(shown[0] < meant[0] - 30.0 && shown[1] < meant[1] - 30.0, "darkened: {shown:?} vs {meant:?}");
        // The darkening is exactly the colour the multiply removed: `ui.rgb * (1 - a)`.
        for i in 0..3 {
            assert!((meant[i] - shown[i] - SUBTITLE[i] as f32 * (1.0 - a)).abs() < 1e-3);
        }
    }

    #[test]
    fn the_pass_runs_over_the_bound_plane_when_the_frame_drew() {
        assert_eq!(plan(true, true, Some(Fetch::Arm), false), Plan::Fetch(Fetch::Arm));
        assert_eq!(plan(true, true, Some(Fetch::Ext), true), Plan::Fetch(Fetch::Ext), "a fetch program wins over a copy one");
        assert_eq!(plan(true, true, None, true), Plan::Copy);
    }

    #[test]
    fn the_pass_does_not_run_off_the_plane_or_on_an_empty_frame() {
        assert_eq!(plan(false, true, Some(Fetch::Arm), true), Plan::Skip(Skip::NoPlane), "an opaque frame needs none");
        assert_eq!(plan(false, false, None, false), Plan::Skip(Skip::NoPlane));
        assert_eq!(
            plan(true, false, Some(Fetch::Arm), true),
            Plan::Skip(Skip::NothingDrawn),
            "the plane presents every frame, nearly all of them a bare clear: no full-viewport pass for those"
        );
        assert_eq!(plan(true, true, None, false), Plan::Skip(Skip::NoProgram));
    }

    /// An extension is matched as a whole token: `..._fetch` must not match the `..._depth_stencil`
    /// extension that merely starts with it, and the string's edges and an empty string are handled.
    #[test]
    fn the_fetch_extension_is_matched_as_a_whole_token() {
        let arm = "GL_EXT_foo GL_ARM_shader_framebuffer_fetch GL_OES_bar";
        assert_eq!(pick_fetch(arm), Some(Fetch::Arm));
        assert_eq!(pick_fetch("GL_ARM_shader_framebuffer_fetch"), Some(Fetch::Arm), "the only entry");
        assert_eq!(pick_fetch("GL_ARM_shader_framebuffer_fetch GL_EXT_shader_framebuffer_fetch"), Some(Fetch::Arm), "ARM first");
        assert_eq!(pick_fetch("GL_EXT_shader_framebuffer_fetch"), Some(Fetch::Ext));
        assert_eq!(pick_fetch("GL_ARM_shader_framebuffer_fetch_depth_stencil"), None, "a prefix is not the extension");
        assert_eq!(pick_fetch("GL_EXT_shader_framebuffer_fetch_non_coherent"), None);
        assert_eq!(pick_fetch(""), None);
        assert_eq!(pick_fetch("GL_ARM_shader_framebuffer_fetch\nGL_OES_x"), Some(Fetch::Arm), "any ASCII whitespace separates");
    }

    /// **The draw ledger.** A frame that drew is told so, [`frame_begin`] starts the next one clean
    /// and lifts the straightened latch, and the latch is what the framebuffer-sampling doors refuse
    /// on.
    #[test]
    fn the_ledger_counts_draws_and_the_frame_start_clears_it() {
        let _g = plx_base::testlock::serial();
        frame_begin();
        assert!(!frame_drew() && !straightened());
        note_draw();
        assert!(frame_drew());
        STRAIGHTENED.with(|s| s.set(true));
        assert!(straightened());
        assert!(super::super::video_plane_refuses("test door"), "a door refuses once the surface is straight");
        frame_begin();
        assert!(!frame_drew() && !straightened());
        assert!(!super::super::video_plane_refuses("test door"), "and is open again on the next frame");
    }

    /// **Every draw is counted**: the raw `glDrawArrays` import is called in exactly one place per
    /// file that draws (its counting wrapper) and by the pass itself, so no primitive can put pixels
    /// on the surface without the ledger knowing. A frame the ledger calls empty must BE empty, or
    /// the pass skips a frame that has a translucent pixel on it and the fault returns without a trace.
    #[test]
    fn the_raw_draw_call_is_reached_only_through_the_counting_wrappers() {
        let calls = |src: &str, prod_end: &str| {
            let prod = &src[..src.find(prod_end).unwrap_or(src.len())];
            prod.lines()
                .filter(|l| !l.trim_start().starts_with("//") && l.contains("glDrawArrays_raw("))
                .filter(|l| !l.contains("fn glDrawArrays_raw"))
                .count()
        };
        assert_eq!(calls(include_str!("../gfx.rs"), "\n#[cfg(test)]\nmod tests"), 1, "gfx.rs: only the wrapper calls the raw import");
        assert_eq!(calls(include_str!("../text.rs"), "\n#[cfg(test)]\nmod tests"), 1, "text.rs: only the wrapper calls the raw import");
        assert_eq!(calls(include_str!("straighten.rs"), "\n#[cfg(test)]\nmod tests"), 1, "the pass's own draw is the one more");
        for (name, src) in [("gfx.rs", include_str!("../gfx.rs")), ("text.rs", include_str!("../text.rs"))] {
            let prod = &src[..src.find("\n#[cfg(test)]\nmod tests").unwrap_or(src.len())];
            let wrapper = prod.find("unsafe fn glDrawArrays(").unwrap_or_else(|| panic!("{name}: the counting wrapper is gone"));
            let body = &prod[wrapper..wrapper + prod[wrapper..].find("\n}\n").expect("its end")];
            assert!(body.contains("note_draw()"), "{name}: the wrapper books the draw before it issues it");
            assert!(
                body.find("note_draw()") < body.find("glDrawArrays_raw("),
                "{name}: …and books it first, so a draw that panics in the driver is still counted"
            );
        }
    }

    /// The shader is the CPU model: the divide is guarded at zero alpha, colour is multiplied by the
    /// reciprocal, alpha passes through, and the two fetch variants name their extension's built-in.
    #[test]
    fn the_shader_is_the_arithmetic_the_model_checks() {
        let fs = include_str!("../shaders/fs_straighten.frag");
        assert!(fs.contains("float k = c.a > 0.0 ? 1.0 / c.a : 0.0;"));
        assert!(fs.contains("gl_FragColor = vec4(c.rgb * k, c.a);"));
        assert!(fs.contains("#define PLX_BEHIND gl_LastFragColorARM"));
        assert!(fs.contains("#define PLX_BEHIND gl_LastFragData[0]"));
        assert!(fs.contains("#extension GL_ARM_shader_framebuffer_fetch : require"));
        assert!(fs.contains("#extension GL_EXT_shader_framebuffer_fetch : require"));
        assert!(fs.contains("texture2D(u_tex, v_uv)"), "the copy variant samples its copy");
    }

    // ---- the extent ledger ---------------------------------------------------------------------------

    /// A small viewport the model tests count pixels on: 192 x 108, with authored space at one tenth.
    const VIEW: PxBox = [0, 0, 192, 108];
    const TENTH: f32 = 0.1;

    /// The box of an authored quad on [`VIEW`].
    fn at_tenth(q: [f32; 4]) -> Option<PxBox> {
        quad_box(q, (0, 0), TENTH)
    }

    /// How many of `boxes` cover each pixel of [`VIEW`], row-major.
    fn coverage(boxes: &[PxBox]) -> Vec<u8> {
        let (w, h) = ((VIEW[2] - VIEW[0]) as usize, (VIEW[3] - VIEW[1]) as usize);
        let mut count = vec![0u8; w * h];
        for b in boxes {
            for y in b[1].max(VIEW[1])..b[3].min(VIEW[3]) {
                for x in b[0].max(VIEW[0])..b[2].min(VIEW[2]) {
                    count[y as usize * w + x as usize] += 1;
                }
            }
        }
        count
    }

    /// A deterministic spread of authored quads: small and large, near and far, some on the edges.
    fn spread(n: usize) -> Vec<[f32; 4]> {
        let mut s = 0x9E37_79B9u32;
        let mut next = move |m: u32| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 8) % m
        };
        (0..n)
            .map(|_| [next(2100) as f32 - 100.0, next(1200) as f32 - 60.0, 1.0 + next(700) as f32, 1.0 + next(300) as f32])
            .collect()
    }

    /// **The pass covers every pixel any draw touched, once, and no other.** A few hundred quads of
    /// every size are booked; each pixel of the viewport is then covered at most once (the fetch
    /// program divides what it finds, so a pixel two quads cover would be divided twice: wrong colour,
    /// and on the television only, because the simulator's copy program reads an untouched copy), and
    /// exactly once wherever a draw's own box reaches. Without this the quads could not be trusted
    /// to touch a translucent pixel at all.
    #[test]
    fn the_ledger_covers_every_drawn_pixel_exactly_once_and_no_other() {
        let quads: Vec<[f32; 4]> = spread(300);
        let mut ledger = Ledger::new();
        let mut drawn = Vec::new();
        for q in &quads {
            if let Some(b) = at_tenth(*q) {
                drawn.push(b);
                ledger.add(b);
            }
        }
        let covering = ledger.covering(VIEW);
        assert!(covering.len() <= SLOTS, "{} boxes", covering.len());
        let (got, want) = (coverage(&covering), coverage(&drawn));
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(*g <= 1, "pixel {i} is covered by {g} quads");
            assert!(*g >= 1 || *w == 0, "pixel {i} was drawn into and is not covered");
        }
    }

    /// The pass is not the screen: a dozen small quads leave most of the viewport alone. (The property
    /// above holds for the whole viewport too; this is the half that says the work went down.)
    #[test]
    fn a_frame_of_small_quads_touches_a_small_part_of_the_surface() {
        let mut ledger = Ledger::new();
        for q in spread(12).iter().map(|q| [q[0], q[1], q[2] / 8.0, q[3] / 8.0]) {
            if let Some(b) = at_tenth(q) {
                ledger.add(b);
            }
        }
        let touched = coverage(&ledger.covering(VIEW)).iter().filter(|c| **c > 0).count();
        assert!(touched * 4 < 192 * 108, "{touched} of {} pixels", 192 * 108);
    }

    /// With room to spare the pass covers exactly the drawn boxes: a frame with two small regions on
    /// it is not widened to the screen, nor to their bounding box.
    #[test]
    fn a_few_separate_quads_are_covered_exactly() {
        let a = at_tenth([100.0, 100.0, 300.0, 60.0]).unwrap();
        let b = at_tenth([1500.0, 800.0, 200.0, 100.0]).unwrap();
        let mut ledger = Ledger::new();
        ledger.add(a);
        ledger.add(b);
        let mut covering = ledger.covering(VIEW);
        covering.sort();
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(covering, want);
        let area = |boxes: &[PxBox]| coverage(boxes).iter().filter(|c| **c > 0).count();
        assert!(area(&covering) < 192 * 108 / 8, "two small regions are a small part of the surface");
    }

    /// A box inside a box already booked adds nothing, and two that share a pixel become one: the
    /// shape every glyph over its scrim takes.
    #[test]
    fn a_contained_box_adds_nothing_and_overlapping_boxes_merge() {
        let mut ledger = Ledger::new();
        ledger.add([10, 10, 100, 60]);
        ledger.add([20, 20, 30, 30]);
        assert_eq!(ledger.covering(VIEW), vec![[10, 10, 100, 60]]);
        ledger.add([90, 50, 150, 100]);
        assert_eq!(ledger.covering(VIEW), vec![[10, 10, 150, 100]], "the overlap folds both into their bounding box");
    }

    /// Boxes that only TOUCH share no pixel and stay two: the pass draws each once, and nothing is
    /// widened over the corner they leave out.
    #[test]
    fn boxes_that_touch_stay_two() {
        let mut ledger = Ledger::new();
        ledger.add([0, 0, 50, 50]);
        ledger.add([50, 0, 100, 50]);
        ledger.add([0, 50, 50, 60]);
        let mut covering = ledger.covering(VIEW);
        covering.sort();
        assert_eq!(covering, vec![[0, 0, 50, 50], [0, 50, 50, 60], [50, 0, 100, 50]]);
    }

    /// Past [`SLOTS`] separate boxes the two that waste the fewest pixels together are folded, and
    /// nothing that was drawn is ever left out.
    #[test]
    fn past_the_slots_the_cheapest_pair_folds() {
        let mut ledger = Ledger::new();
        let far: Vec<PxBox> = (0..SLOTS as i32).map(|i| [i * 11, 100 - i, i * 11 + 4, 104 - i]).collect();
        for b in &far {
            ledger.add(*b);
        }
        assert_eq!(ledger.covering(VIEW).len(), SLOTS);
        // A seventeenth box two pixels from the first one: they are the cheapest pair.
        ledger.add([0, 96, 4, 100]);
        let covering = ledger.covering(VIEW);
        assert_eq!(covering.len(), SLOTS, "still at most {SLOTS} boxes");
        for b in &far {
            assert!(covering.iter().any(|c| c[0] <= b[0] && c[1] <= b[1] && c[2] >= b[2] && c[3] >= b[3]), "{b:?} is not covered");
        }
        assert!(covering.contains(&[0, 96, 4, 104]) || covering.iter().any(|c| c[0] == 0 && c[1] <= 96), "the new box folded into its neighbour");
        assert!(coverage(&covering).iter().all(|c| *c <= 1), "and no two boxes share a pixel");
    }

    /// **An extent that is not known is the whole surface.** A draw with no rect to place it, or one
    /// whose rect is not a number, books the box that holds every viewport, and every later box folds
    /// into it.
    #[test]
    fn an_unknown_extent_is_the_whole_surface() {
        let mut ledger = Ledger::new();
        ledger.add([10, 10, 20, 20]);
        ledger.add(EVERYTHING);
        ledger.add([30, 30, 40, 40]);
        assert_eq!(ledger.covering(VIEW), vec![VIEW]);
        assert_eq!(quad_box([f32::NAN, 0.0, 10.0, 10.0], (0, 0), 1.0), Some(EVERYTHING));
        assert_eq!(quad_box([0.0, 0.0, f32::INFINITY, 10.0], (0, 0), 1.0), Some(EVERYTHING));
        assert_eq!(quad_box([0.0, 0.0, 10.0, 10.0], (0, 0), 0.0), Some(EVERYTHING), "no scale is no mapping");
    }

    /// A quad becomes the window box it can reach: y is flipped (GL's rows count upward), the box is
    /// rounded OUTWARD and given a pixel of slack, and the viewport's origin and scale are applied.
    #[test]
    fn a_quad_is_the_window_box_it_can_reach() {
        assert_eq!(quad_box([100.0, 200.0, 300.0, 50.0], (0, 0), 1.0), Some([99, 829, 401, 881]));
        assert_eq!(quad_box([100.5, 200.25, 10.0, 10.0], (0, 0), 1.0), Some([99, 868, 112, 881]), "outward on both axes");
        assert_eq!(quad_box([100.0, 200.0, 300.0, 50.0], (10, 20), 0.5), Some([59, 434, 211, 461]), "origin and scale");
        assert_eq!(quad_box([100.0, 200.0, -300.0, 50.0], (0, 0), 1.0), quad_box([-200.0, 200.0, 300.0, 50.0], (0, 0), 1.0), "a flipped quad is the same box");
        assert_eq!(quad_box([5.0, 5.0, 0.0, 10.0], (0, 0), 1.0), None, "no width, no pixels");
        assert_eq!(quad_box([5.0, 5.0, 10.0, 0.0], (0, 0), 1.0), None, "no height, no pixels");
    }

    /// **The pass's quads are the boxes and only them**: each quad's `u_box` maps back to a box of
    /// the ledger cut to the viewport, a box wholly outside the viewport draws nothing, and the
    /// pixels the quads cover are the pixels the ledger holds, no more.
    #[test]
    fn the_pass_draws_quads_only_over_the_ledger() {
        let mut ledger = Ledger::new();
        ledger.add([20, 30, 80, 50]);
        ledger.add([150, 90, 260, 400]); // runs off the right and top edges
        ledger.add([-90, -90, -10, -10]); // wholly outside
        let q = quads(&ledger, VIEW);
        assert_eq!(q.len(), 2, "the box outside the viewport is not drawn");
        let back = |q: &[f32; 4]| [(q[0] * 192.0).round() as i32, (q[1] * 108.0).round() as i32, (q[2] * 192.0).round() as i32, (q[3] * 108.0).round() as i32];
        let mut boxes: Vec<PxBox> = q.iter().map(back).collect();
        boxes.sort();
        assert_eq!(boxes, vec![[20, 30, 80, 50], [150, 90, 192, 108]]);
        let covered = coverage(&boxes);
        assert_eq!(covered.iter().filter(|c| **c > 0).count(), 60 * 20 + 42 * 18);
        assert!(covered.iter().all(|c| *c <= 1));
    }

    /// An empty ledger has nothing to cover: the pass is not run for it.
    #[test]
    fn an_empty_ledger_covers_nothing() {
        let ledger = Ledger::new();
        assert!(ledger.is_empty());
        assert!(quads(&ledger, VIEW).is_empty());
    }

    /// The wiring no host test can drive: `init_gl` links the pass's programs.
    #[test]
    fn init_gl_links_the_pass() {
        let src = include_str!("../gfx.rs");
        let from = src.find("pub fn init_gl()").expect("init_gl");
        let body = &src[from..from + src[from..].find("\n}\n").expect("its end")];
        assert!(body.contains("straighten::init()"), "init_gl must link the straighten programs");
    }
}
