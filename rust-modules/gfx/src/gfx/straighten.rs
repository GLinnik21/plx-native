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
//! A last draw before the swap rewrites the frame `rgb' = a > 0 ? rgb / a : 0`, alpha unchanged,
//! blending off (`fs_straighten.frag`), over the pixels the frame drew into and no others (below). The
//! compositor's multiply then returns exactly the colour the frame drew: `(rgb / a) * a + video * (1 - a)
//! = rgb + video * (1 - a)`, the premultiplied picture the frame was built as. An opaque pixel (`a = 1`)
//! and a hole (`a = 0`) are fixed points, which is what makes "the pixels the frame drew into" enough:
//! every other pixel is the transparent clear.
//!
//! **Where it runs.** From the application loop (`app/run.rs`'s `present_and_swap`), after the frame's
//! draw and its one framebuffer readback and before anything else touches the surface, and only on a
//! frame whose surface is over the BOUND video plane (`Player::video_plane_bound`) that DREW something.
//! The gate presents every frame while the plane is bound, and about 99 % of those frames are a
//! transparent clear with no draw call at all. Two reads of the frame's own state decide it: the plane
//! bit (a function argument, not gfx state: [`super::video_plane_frame`] is restored to `false` before
//! the draw returns, and it excludes the bound trailer preview, a `RenderStrategy::Page` screen whose
//! chrome is just as translucent) and [`frame_drew`], whether [`note_draw`] booked a draw onto the
//! surface since [`frame_begin`].
//!
//! **Where it draws.** One quad over each box of the surface the frame drew into, and nowhere else. A
//! pass over the whole viewport (2.07 M pixels, about 40 fps where the frame ran at about 50 on the dev
//! set, the same fixture and the release build) touched every tile of a frame that is, for the length of
//! a film, a subtitle line and a clear. Every draw call in this crate says what it writes
//! ([`Extent`]): the authored-pixel quad its program places the unit quad over ([`super::set_rect`] sets
//! that uniform and returns the extent, so the two cannot differ), or the whole target for a draw with no
//! rect. [`note_draw`] turns it into a window-pixel box, rounded outward with a pixel of slack for the
//! vertex stage's rounding, and books it in a [`Ledger`]: at most 16 boxes, kept PAIRWISE DISJOINT by
//! folding two that share a pixel into their bounding box (and, past 16, the pair that wastes the fewest
//! pixels). Disjoint is not tidiness. The fetch program rewrites the pixel it finds, so a pixel two quads
//! cover would be divided by its alpha twice; the copy program reads an untouched copy and would not
//! show it, which is why the simulator cannot catch an overlap and a host model test pins it
//! (`the_ledger_covers_every_drawn_pixel_exactly_once_and_no_other`). Boxes that only touch stay two:
//! the rasterizer's fill rule gives their shared edge to exactly one of them.
//!
//! **What books nothing, and what books everything.** A draw into a target of gfx's own (a blur pass, a
//! field reduction, a region fill; [`note_bind`] knows from the binds, and a host-page snapshot by
//! `default_fb`) is not the surface's and books nothing; the draw that puts its pixels on the surface
//! books its own quad. A draw with no known extent ([`Extent::Target`], or a quad that is not a
//! number) books the whole surface and the pass covers the viewport, as it did before it knew where the
//! frame drew. A `glClear` is not a draw ([`note_clear`]): the two alphas the app clears the surface to,
//! 0 and 1, are fixed points, and any other alpha books the whole surface. The scissor is ignored: it
//! only narrows what a quad writes, so a box that ignores it is a superset.
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
//! colour: it must clear every damaged region first and run this pass over exactly that region (the
//! ledger already holds the boxes a frame drew; it would be reset to the damage instead of to nothing).
//! The same fact is why untouched pixels may be left alone: a frame starts from a clear, so a pixel no
//! draw booked is `(0, 0, 0, 0)`.
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
//! A fragment program with a reciprocal and a multiply per pixel (about 2 arithmetic words a quad on
//! Midgard) over the covered pixels, and a draw call per box (at most 16). What the covered pixels are
//! on a frame follows the frame. MODELLED, not measured, by running `Ledger::add` over assumed 1080p
//! frames: two dialogue lines about 0.15 M pixels, a heavy opening whose regions sit in the lower 40% of
//! the screen about 0.9 M, a HUD with full-width top and bottom scrims about 1.2 M; 64 regions scattered
//! over the whole frame fold to one bounding box of nearly all of it, and a draw with no extent is all
//! 2.07 M (the pass then costs what it did before). The pass over the whole viewport was measured on the
//! set as the loss of about 10 fps (about 50 to about 40 on the UI heartbeat); the covered pixels are
//! the figure that scales it, and the pass's cost over a given set of boxes is UNMEASURED on the set.
//! The copy fallback adds the frame's copy and a pass reload (about 17 MB of traffic at 1080p), whatever
//! the boxes. The boxes are the drawn extents, not the logical rects (`AA_BLEED`, shadow and glow pads
//! are inside them), and a bounding box of a few draws can cover pixels none of them touched: those are
//! the clear, a fixed point.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint};
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use super::{
    cap_tex, delete_tex, glActiveTexture, glBindFramebuffer, glBindTexture, glCopyTexSubImage2D, glDisable,
    glDrawArrays_raw, glEnable, glGetString, glGetUniformLocation, glTexParameteri, glUniform4f, glViewport,
    link_optional_program, scissor_off, use_prog, FS_STRAIGHTEN_ARM, FS_STRAIGHTEN_COPY, FS_STRAIGHTEN_EXT,
    GL_BLEND, GL_FRAMEBUFFER, GL_NEAREST, GL_TEXTURE0, GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER,
    GL_TEXTURE_MIN_FILTER, GL_TRIANGLE_STRIP, VS_STRAIGHTEN,
};
use plx_base::eventlog::log;

// ---- the ledger of draws: "what did this frame put on the surface, and where?" ------------------

thread_local! {
    /// The boxes of the presented surface this frame drew into ([`Ledger`]), reset by [`frame_begin`].
    ///
    /// THREAD-LOCAL like [`STRAIGHTENED`] below and `VIDEO_PLANE_FRAME` beside the doors that read
    /// it: it is draw state, the draw is one thread's, and a process-wide ledger would let one
    /// test's frame decide another's pass.
    static LEDGER: std::cell::RefCell<Ledger> = const { std::cell::RefCell::new(Ledger::new()) };
    /// Is gfx drawing into a target of its own (a blur pass, a field reduction, a region fill) and not
    /// into the surface? Set by [`note_bind`], which every `glBindFramebuffer` in this crate calls.
    static OFFSCREEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Does a draw issued now write the surface the frame is presented from? Two reads of state the
/// crate already keeps, no GL query: gfx's own last bind named the "back to the screen" framebuffer
/// ([`note_bind`]), and no host-page snapshot is being drawn ([`plx_base::surface::PageTarget`]
/// redirects `default_fb` off `frame_fb`, and binds on its own, below this crate's wrapper).
fn presented() -> bool {
    !OFFSCREEN.with(|o| o.get()) && plx_base::surface::default_fb() == plx_base::surface::frame_fb()
}

/// Book one draw call: the extent it writes, if it writes the presented surface. Every `glDrawArrays`
/// in this crate goes through the wrapper that calls this (`gfx.rs` and `text.rs` each shadow the
/// extern with one that REQUIRES an [`Extent`], and a test pins that the raw import is called nowhere
/// else), so a primitive can neither draw nor be built without saying where it draws.
///
/// A draw into another target books nothing: its pixels are not the surface's, and the draw that puts
/// them there (a glass composite, a region quad) books its own quad. A quad with no area books nothing
/// either. The render thread is the only caller.
#[inline]
pub(crate) fn note_draw(extent: Extent) {
    if !presented() {
        return;
    }
    let b = match extent {
        Extent::Target => Some(EVERYTHING),
        Extent::Quad(q) => {
            let (vx, vy, _, _) = plx_base::surface::viewport();
            quad_box(q, (vx, vy), plx_base::surface::scale())
        }
    };
    if let Some(b) = b {
        LEDGER.with(|l| l.borrow_mut().add(b));
    }
}

/// Note that gfx bound framebuffer `fb`: every `glBindFramebuffer` in this crate calls this first. A
/// bind of the "back to the screen" framebuffer ([`plx_base::surface::default_fb`], which every pass
/// that leaves the surface ends with) is the surface again; any other bind is a target of gfx's own.
#[inline]
pub(crate) fn note_bind(fb: c_uint) {
    OFFSCREEN.with(|o| o.set(fb != plx_base::surface::default_fb()));
}

/// Book a `glClear` of the colour with this `alpha`. A clear is not a draw, and it is sound to leave
/// it out only because the two alphas the app clears the surface to are fixed points of the pass: 0
/// (the transparent hole every frame over the plane starts from) and 1 (an opaque ground). Any other
/// alpha would leave the whole surface translucent and the pass has to cover all of it.
#[inline]
pub(crate) fn note_clear(alpha: f32) {
    if presented() && !(alpha <= 0.0 || alpha >= 1.0) {
        LEDGER.with(|l| l.borrow_mut().add(EVERYTHING));
    }
}

/// Open a drawn frame: nothing has been drawn yet and the surface is not straightened. Called beside
/// `text::begin_frame`, once per presented frame, before the draw.
pub fn frame_begin() {
    LEDGER.with(|l| *l.borrow_mut() = Ledger::new());
    OFFSCREEN.with(|o| o.set(false));
    STRAIGHTENED.with(|s| s.set(false));
}

/// Has this frame drawn anything onto the presented surface since [`frame_begin`]?
pub fn frame_drew() -> bool {
    LEDGER.with(|l| !l.borrow().is_empty())
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
    /// Nothing reached the surface since the clear: the frame is transparent black, a fixed point.
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
/// Where the linked program keeps `u_box`, the box a quad covers (`vs_straighten.vert`); -1 with none.
static BOX_LOC: AtomicI32 = AtomicI32::new(-1);
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

/// Link the pass's vertex shader with the fragment source `fs` and find the `u_box` it draws each box
/// through: `None` when either fails, so a program the pass could not place a quad with is never kept.
fn link_pass(fs: &CStr) -> Option<(c_uint, c_int)> {
    let p = link_optional_program(VS_STRAIGHTEN.as_ptr(), fs.as_ptr())?;
    // SAFETY: the render thread, in the context that linked `p`; a name lookup.
    let loc = unsafe { glGetUniformLocation(p, c"u_box".as_ptr()) };
    (loc >= 0).then_some((p, loc))
}

/// Link the pass's program for this driver, once per GL context. Called by `init_gl`; a failure is
/// logged and leaves the pass off (or on its copy program), never a boot failure: a frame that goes
/// out premultiplied is the picture this app has always shown.
pub fn init() {
    COPY_TEX.store(0, Ordering::Relaxed);
    COPY_W.store(0, Ordering::Relaxed);
    COPY_H.store(0, Ordering::Relaxed);
    let (mut fetch_prog, mut kind, mut box_loc) = (0, 0, -1);
    if let Some(f) = pick_fetch(&gl_extensions()) {
        let fs = match f {
            Fetch::Arm => FS_STRAIGHTEN_ARM,
            Fetch::Ext => FS_STRAIGHTEN_EXT,
        };
        match link_pass(fs) {
            Some((p, loc)) => {
                (fetch_prog, box_loc) = (p, loc);
                kind = if f == Fetch::Arm { 1 } else { 2 };
            }
            None => log(&format!("straighten: the {f:?} framebuffer-fetch program failed to build - copying instead")),
        }
    }
    let copy_prog = if fetch_prog != 0 {
        0
    } else {
        link_pass(FS_STRAIGHTEN_COPY).map_or(0, |(p, loc)| {
            box_loc = loc;
            p
        })
    };
    FETCH_PROG.store(fetch_prog, Ordering::Relaxed);
    FETCH_KIND.store(kind, Ordering::Relaxed);
    COPY_PROG.store(copy_prog, Ordering::Relaxed);
    BOX_LOC.store(box_loc, Ordering::Relaxed);
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
/// It draws one quad over each box the frame drew into ([`Ledger`]) and over nothing else: a pixel no
/// draw touched is the transparent clear, a fixed point, and the boxes are disjoint, so every pixel the
/// pass visits it visits once. A box is a quad of its own and not a scissor: on this GPU a scissor
/// bounds what a pass may WRITE and the tiler still walks every tile under the geometry
/// (`gfx::culled`'s measurement), so only the quad's size removes the work.
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
    let ledger = LEDGER.with(|l| *l.borrow());
    let mut boxes = quads(&ledger, [vx, vy, vx + vw, vy + vh]);
    // Every box may lie off the viewport (a draw wholly outside it): nothing to straighten.
    let Some(first) = boxes.next() else { return false };
    let loc = BOX_LOC.load(Ordering::Relaxed);
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
        for q in std::iter::once(first).chain(boxes) {
            glUniform4f(loc, q[0], q[1], q[2], q[3]);
            glDrawArrays_raw(GL_TRIANGLE_STRIP, 0, 4);
        }
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

/// **The boxes of the surface a frame drew into**, at most [`SLOTS`] of them and PAIRWISE DISJOINT.
///
/// Disjoint is the contract the pass leans on: its fetch program rewrites the pixel it finds, so a
/// pixel two quads cover is divided by its alpha twice. [`Ledger::add`] keeps it as an invariant (two
/// boxes that share a pixel become their bounding box, which may now share one with a third, and so
/// on); boxes that only touch share nothing and stay two.
#[derive(Clone, Copy)]
struct Ledger {
    boxes: [PxBox; SLOTS],
    n: usize,
}

fn overlaps(a: PxBox, b: PxBox) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

fn contains(a: PxBox, b: PxBox) -> bool {
    a[0] <= b[0] && a[1] <= b[1] && b[2] <= a[2] && b[3] <= a[3]
}

fn union(a: PxBox, b: PxBox) -> PxBox {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

fn area(b: PxBox) -> i64 {
    (b[2] - b[0]) as i64 * (b[3] - b[1]) as i64
}

impl Ledger {
    const fn new() -> Self {
        Ledger { boxes: [[0; 4]; SLOTS], n: 0 }
    }

    fn is_empty(&self) -> bool {
        self.n == 0
    }

    fn boxes(&self) -> &[PxBox] {
        &self.boxes[..self.n]
    }

    fn remove(&mut self, i: usize) {
        self.n -= 1;
        self.boxes[i] = self.boxes[self.n];
    }

    /// Book a box the frame drew into. Every call either returns having covered `b` or removes a box,
    /// so it ends; every pixel of every box ever booked stays inside one of the boxes held.
    fn add(&mut self, mut b: PxBox) {
        loop {
            if self.boxes().iter().any(|&e| contains(e, b)) {
                return;
            }
            // A box that shares a pixel with `b` is folded into it, and `b` looks again: the union
            // may now share one with another.
            if let Some(i) = self.boxes().iter().position(|&e| overlaps(e, b)) {
                b = union(self.boxes[i], b);
                self.remove(i);
                continue;
            }
            if self.n < SLOTS {
                self.boxes[self.n] = b;
                self.n += 1;
                return;
            }
            // Full, and `b` is apart from all of them: fold it into the box it widens the least, the
            // pair whose bounding box wastes the fewest pixels (what is covered beyond the two).
            let waste = |&e: &PxBox| area(union(e, b)) - area(e) - area(b);
            let i = (0..self.n).min_by_key(|&i| waste(&self.boxes[i])).expect("SLOTS is not zero");
            b = union(self.boxes[i], b);
            self.remove(i);
        }
    }

    /// The boxes the pass draws over: the ledger's, cut to the viewport `view`, a box with nothing
    /// left of it dropped. Cutting keeps them disjoint.
    fn covering(&self, view: PxBox) -> impl Iterator<Item = PxBox> + '_ {
        self.boxes().iter().filter_map(move |&b| super::clip_px(b, Some(view)))
    }
}

/// The pass's quads for `ledger` over the viewport `view`: each box as the `(x0, y0, x1, y1)`
/// fractions of the viewport (`0, 0` its bottom-left) the vertex shader's `u_box` takes. Edges are whole
/// pixels over the viewport's size, so two boxes that touch compute the same fraction for the edge
/// they share and the rasterizer's fill rule gives it to exactly one of them.
fn quads(ledger: &Ledger, view: PxBox) -> impl Iterator<Item = [f32; 4]> + '_ {
    let (w, h) = ((view[2] - view[0]) as f32, (view[3] - view[1]) as f32);
    ledger.covering(view).map(move |b| {
        [(b[0] - view[0]) as f32 / w, (b[1] - view[1]) as f32 / h, (b[2] - view[0]) as f32 / w, (b[3] - view[1]) as f32 / h]
    })
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

    /// The boxes the frame's ledger holds on the real (thread-local) ledger, on [`VIEW`]'s own scale:
    /// the surface's viewport is not touched by this crate's tests, so a draw books at the identity
    /// mapping. Cleared by [`frame_begin`].
    fn booked() -> Vec<PxBox> {
        let view = {
            let (vx, vy, vw, vh) = plx_base::surface::viewport();
            [vx, vy, vx + vw, vy + vh]
        };
        LEDGER.with(|l| l.borrow().covering(view).collect())
    }

    /// **The draw ledger.** A frame that drew onto the surface is told so, [`frame_begin`] starts the
    /// next one clean and lifts the straightened latch, and the latch is what the
    /// framebuffer-sampling doors refuse on.
    #[test]
    fn the_ledger_books_draws_and_the_frame_start_clears_it() {
        let _g = plx_base::testlock::serial();
        frame_begin();
        assert!(!frame_drew() && !straightened());
        note_draw(Extent::quad(100.0, 100.0, 200.0, 50.0));
        assert!(frame_drew());
        assert_eq!(booked().len(), 1);
        STRAIGHTENED.with(|s| s.set(true));
        assert!(straightened());
        assert!(super::super::video_plane_refuses("test door"), "a door refuses once the surface is straight");
        frame_begin();
        assert!(!frame_drew() && !straightened() && booked().is_empty());
        assert!(!super::super::video_plane_refuses("test door"), "and is open again on the next frame");
    }

    /// **Only a draw onto the presented surface is booked.** A blur pass, a field reduction or a region
    /// fill draws into a target of gfx's own, between a bind of it and the bind back to
    /// `default_fb` (every pass ends that way, and `note_bind` reads it), and none of its pixels is the
    /// surface's. A frame that did nothing else has nothing to straighten.
    #[test]
    fn a_draw_into_another_target_is_not_booked() {
        let _g = plx_base::testlock::serial();
        frame_begin();
        note_bind(7);
        note_draw(Extent::quad(0.0, 0.0, 1920.0, 1080.0));
        note_draw(Extent::Target);
        note_clear(0.5);
        assert!(!frame_drew(), "draws and a clear into a target that is not the surface book nothing");
        note_bind(plx_base::surface::default_fb());
        note_draw(Extent::quad(10.0, 10.0, 20.0, 20.0));
        assert!(frame_drew(), "and the bind back to the surface books again");
        frame_begin();
    }

    /// **A draw with no rect is the whole surface**, and so is a quad that is not a number: the pass
    /// then covers the viewport, which is what it did before it knew where the frame drew.
    #[test]
    fn a_draw_without_an_extent_books_the_whole_surface() {
        let _g = plx_base::testlock::serial();
        frame_begin();
        note_draw(Extent::quad(100.0, 100.0, 10.0, 10.0));
        note_draw(Extent::Target);
        let (vx, vy, vw, vh) = plx_base::surface::viewport();
        assert_eq!(booked(), vec![[vx, vy, vx + vw, vy + vh]], "the viewport, as one box");
        frame_begin();
        note_draw(Extent::quad(f32::NAN, 0.0, 10.0, 10.0));
        assert_eq!(booked(), vec![[vx, vy, vx + vw, vy + vh]]);
        frame_begin();
    }

    /// **A clear is booked only when the pass would not leave its pixels alone.** The two alphas the
    /// app clears the surface to (0, the hole; 1, an opaque ground) are fixed points. Anything between
    /// would leave every pixel translucent, and any other value of the colour channel is not asked.
    #[test]
    fn a_translucent_clear_books_the_whole_surface_and_the_two_the_app_makes_book_nothing() {
        let _g = plx_base::testlock::serial();
        frame_begin();
        note_clear(0.0);
        note_clear(1.0);
        assert!(!frame_drew(), "a transparent hole and an opaque ground need no straightening");
        note_clear(0.5);
        assert!(frame_drew());
        let (vx, vy, vw, vh) = plx_base::surface::viewport();
        assert_eq!(booked(), vec![[vx, vy, vx + vw, vy + vh]]);
        frame_begin();
        note_clear(f32::NAN);
        assert!(frame_drew(), "an alpha that is not a number is not known to be a fixed point");
        frame_begin();
    }

    /// **Every draw says where it lands**: the raw `glDrawArrays` import is called in exactly one place
    /// per file that draws (its wrapper, which requires an [`Extent`]) and by the pass itself, so no
    /// primitive can put pixels on the surface without the ledger knowing where. A box the ledger
    /// misses is a translucent pixel the pass skips and the fault returns without a trace. The two
    /// other calls that decide what the pass covers (`glBindFramebuffer`: which target; `glClear`: a
    /// translucent ground) are shadowed the same way, each in `gfx.rs` alone.
    #[test]
    fn the_raw_calls_are_reached_only_through_the_wrappers_that_book_them() {
        let calls = |src: &str, raw: &str| {
            let prod = &src[..src.find("\n#[cfg(test)]\nmod tests").unwrap_or(src.len())];
            prod.lines()
                .filter(|l| !l.trim_start().starts_with("//") && l.contains(&format!("{raw}(")))
                .filter(|l| !l.contains(&format!("fn {raw}")))
                .count()
        };
        let (gfx, text, pass) = (include_str!("../gfx.rs"), include_str!("../text.rs"), include_str!("straighten.rs"));
        assert_eq!(calls(gfx, "glDrawArrays_raw"), 1, "gfx.rs: only the wrapper calls the raw import");
        assert_eq!(calls(text, "glDrawArrays_raw"), 1, "text.rs: only the wrapper calls the raw import");
        assert_eq!(calls(pass, "glDrawArrays_raw"), 1, "the pass's own draw is the one more");
        assert_eq!(calls(gfx, "glClear_raw"), 1, "gfx.rs: only the wrapper clears");
        assert_eq!(calls(gfx, "glBindFramebuffer_raw"), 1, "gfx.rs: only the wrapper binds");
        assert_eq!(calls(text, "glClear_raw") + calls(text, "glBindFramebuffer_raw"), 0, "text.rs binds and clears nothing");
        for (name, src, raw, note) in [
            ("gfx.rs", gfx, "glDrawArrays", "note_draw(extent)"),
            ("text.rs", text, "glDrawArrays", "note_draw(extent)"),
            ("gfx.rs", gfx, "glClear", "note_clear("),
            ("gfx.rs", gfx, "glBindFramebuffer", "note_bind("),
        ] {
            let prod = &src[..src.find("\n#[cfg(test)]\nmod tests").unwrap_or(src.len())];
            let head = format!("unsafe fn {raw}(");
            let wrapper = prod.find(&head).unwrap_or_else(|| panic!("{name}: the {raw} wrapper is gone"));
            let body = &prod[wrapper..wrapper + prod[wrapper..].find("\n}\n").expect("its end")];
            assert!(body.contains(note), "{name}: the {raw} wrapper books the call");
            assert!(
                body.find(note) < body.find(&format!("{raw}_raw(")),
                "{name}: …and books it first, so a call that panics in the driver is still counted"
            );
        }
        assert!(include_str!("../gfx.rs").contains("extent: straighten::Extent) {"), "the draw wrapper REQUIRES the extent");
    }

    /// **The pass draws only over the ledger's boxes**: its loop is over [`quads`], one `u_box` and one
    /// draw each, and the full-viewport draw it had is gone.
    #[test]
    fn the_pass_loops_over_the_ledgers_quads() {
        let src = include_str!("straighten.rs");
        let prod = &src[..src.find("\n#[cfg(test)]\nmod tests").expect("the unit tests")];
        let from = prod.find("pub fn straighten_surface(").expect("the pass");
        let body = &prod[from..from + prod[from..].find("\n}\n").expect("its end")];
        assert!(body.contains("quads(&ledger,"), "the quads come from the ledger");
        assert!(body.contains("for q in std::iter::once(first).chain(boxes) {"), "one draw per quad");
        let draw = body.find("glDrawArrays_raw(").expect("the draw");
        assert!(body[..draw].rfind("glUniform4f(loc, q[0], q[1], q[2], q[3]);").is_some(), "each draw sets its own box first");
        assert_eq!(body.matches("glDrawArrays_raw(").count(), 1);
        let vs = include_str!("../shaders/vs_straighten.vert");
        assert!(vs.contains("uniform highp vec4 u_box;") && vs.contains("v_uv = mix(u_box.xy, u_box.zw, a_pos);"));
        assert!(vs.contains("gl_Position = vec4(v_uv * 2.0 - 1.0, 0.0, 1.0);"), "the box is placed as the fractions of the viewport");
    }

    /// The rows of the field mesh cover the part of the rect they span, and a whole-target extent has
    /// no rect to cut.
    #[test]
    fn the_rows_of_the_field_mesh_book_the_part_of_the_rect_they_span() {
        use super::super::{rows_extent, FIELD_N};
        let whole = Extent::quad(10.0, 100.0, 200.0, 160.0);
        assert_eq!(rows_extent(whole, &(0..FIELD_N)), whole, "every row is the rect itself");
        assert_eq!(rows_extent(whole, &(0..FIELD_N / 2)), Extent::quad(10.0, 100.0, 200.0, 80.0), "the top half");
        assert_eq!(rows_extent(whole, &(FIELD_N / 2..FIELD_N)), Extent::quad(10.0, 180.0, 200.0, 80.0), "the bottom half");
        assert_eq!(rows_extent(Extent::Target, &(2..3)), Extent::Target);
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

    /// The boxes the pass draws over for `ledger` on [`VIEW`].
    fn over_view(ledger: &Ledger) -> Vec<PxBox> {
        ledger.covering(VIEW).collect()
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
        let covering = over_view(&ledger);
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
        let touched = coverage(&over_view(&ledger)).iter().filter(|c| **c > 0).count();
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
        let mut covering = over_view(&ledger);
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
        assert_eq!(over_view(&ledger), vec![[10, 10, 100, 60]]);
        ledger.add([90, 50, 150, 100]);
        assert_eq!(over_view(&ledger), vec![[10, 10, 150, 100]], "the overlap folds both into their bounding box");
    }

    /// Boxes that only TOUCH share no pixel and stay two: the pass draws each once, and nothing is
    /// widened over the corner they leave out.
    #[test]
    fn boxes_that_touch_stay_two() {
        let mut ledger = Ledger::new();
        ledger.add([0, 0, 50, 50]);
        ledger.add([50, 0, 100, 50]);
        ledger.add([0, 50, 50, 60]);
        let mut covering = over_view(&ledger);
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
        assert_eq!(over_view(&ledger).len(), SLOTS);
        // A seventeenth box two pixels from the first one: they are the cheapest pair.
        ledger.add([0, 96, 4, 100]);
        let covering = over_view(&ledger);
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
        assert_eq!(over_view(&ledger), vec![VIEW]);
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
        let q: Vec<[f32; 4]> = quads(&ledger, VIEW).collect();
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
        assert!(quads(&ledger, VIEW).next().is_none());
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
