//! GLES2 rendering foundation (was src/gfx.c). Three shader programs (SDF
//! rrect/tri/focus, 4-corner ambient gradient, textured RGBA), the draw primitives,
//! the spring helper, and the seven-segment FPS digits. All GLES2 calls; state is
//! main-thread statics. link_program/use_prog are also used by text.rs (crate path).
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::surface::{LOGICAL_H as SCR_H, LOGICAL_W as SCR_W};

// Per-frame counters for the frame-drop detector: how many card composites are actually issued
// (`draw_tex_carded`), and how many of those are (partly) off-screen — to confirm the cull is tight.
static CARD_CT: AtomicU32 = AtomicU32::new(0);
static CARD_OFF: AtomicU32 = AtomicU32::new(0);
/// (card composites drawn, of which fully+partly off-screen) since the last call; resets both.
pub(crate) fn take_card_stats() -> (u32, u32) {
    (CARD_CT.swap(0, Ordering::Relaxed), CARD_OFF.swap(0, Ordering::Relaxed))
}

/// SDF edge headroom, in px. The fill AA is `1 - smoothstep(-1, 1, d)` — a 2px band centred on the
/// shape edge (`d = 0`). Its outer half (`d ∈ [0, 1]`, alpha 0.5→0) lies OUTSIDE the shape, so unless
/// the drawn quad extends past the edge those fragments are never rasterised and the edge aliases
/// (asymmetrically, at the mercy of each edge's subpixel alignment — the "zipper" on one side). We
/// inflate the quad by this much on every side (and bump `u_pad` to match, keeping `hsz` unchanged)
/// so the falloff has geometry to fade into. 1px exactly covers the band's outer half.
const AA_BLEED: f32 = 1.0;

// Shader sources live in `shaders/*.vert`/`*.frag`, embedded at compile time (`include_str!`) so
// the binary stays self-contained — nothing is loaded at runtime. Each file carries its own docs;
// the family-wide highp/mediump rule is the PRECISION note in `shaders/fs_src.frag`.
macro_rules! glsl {
    ($file:literal) => {
        // SAFETY: GLSL sources contain no interior NUL; concat! appends the terminator.
        unsafe { ::std::ffi::CStr::from_bytes_with_nul_unchecked(concat!(include_str!($file), "\0").as_bytes()) }
    };
}
pub(crate) use glsl;

const VS_SRC: &CStr = glsl!("shaders/vs_src.vert");
const FS_SRC: &CStr = glsl!("shaders/fs_src.frag");
const FS_AMBIENT: &CStr = glsl!("shaders/fs_ambient.frag");
const FS_SHADOW: &CStr = glsl!("shaders/fs_shadow.frag");
const VS_IMG: &CStr = glsl!("shaders/vs_img.vert");
const FS_IMG: &CStr = glsl!("shaders/fs_img.frag");
const FS_BLUR: &CStr = glsl!("shaders/fs_blur.frag");
const FS_GLASS: &CStr = glsl!("shaders/fs_glass.frag");
const GL_VERTEX_SHADER: c_uint = 0x8B31;
const GL_FRAGMENT_SHADER: c_uint = 0x8B30;
const GL_COMPILE_STATUS: c_uint = 0x8B81;
/// `glGetString` name for the driver's GLSL version — what `glsl_preamble` reads.
const GL_SHADING_LANGUAGE_VERSION: c_uint = 0x8B8C;
const GL_LINK_STATUS: c_uint = 0x8B82;
const GL_ARRAY_BUFFER: c_uint = 0x8892;
const GL_STATIC_DRAW: c_uint = 0x88E4;
const GL_FLOAT: c_uint = 0x1406;
const GL_FALSE: u8 = 0;
const GL_TRIANGLE_STRIP: c_uint = 0x0005;
const GL_BLEND: c_uint = 0x0BE2;
const GL_DITHER: c_uint = 0x0BD0;
const GL_ONE: c_uint = 0x0001;
const GL_SRC_ALPHA: c_uint = 0x0302;
const GL_ONE_MINUS_SRC_ALPHA: c_uint = 0x0303;
const GL_TEXTURE_2D: c_uint = 0x0DE1;
const GL_TEXTURE0: c_uint = 0x84C0;

extern "C" {
    fn glGetString(name: c_uint) -> *const c_char;
    fn glCreateShader(ty: c_uint) -> c_uint;
    fn glShaderSource(shader: c_uint, count: c_int, string: *const *const c_char, length: *const c_int);
    fn glCompileShader(shader: c_uint);
    fn glGetShaderiv(shader: c_uint, pname: c_uint, params: *mut c_int);
    fn glGetShaderInfoLog(shader: c_uint, bufsize: c_int, length: *mut c_int, infolog: *mut c_char);
    fn glCreateProgram() -> c_uint;
    fn glAttachShader(program: c_uint, shader: c_uint);
    fn glBindAttribLocation(program: c_uint, index: c_uint, name: *const c_char);
    fn glLinkProgram(program: c_uint);
    fn glGetProgramiv(program: c_uint, pname: c_uint, params: *mut c_int);
    fn glUseProgram(program: c_uint);
    fn glGetUniformLocation(program: c_uint, name: *const c_char) -> c_int;
    fn glUniform4f(loc: c_int, x: f32, y: f32, z: f32, w: f32);
    fn glUniform2f(loc: c_int, x: f32, y: f32);
    fn glUniform3f(loc: c_int, x: f32, y: f32, z: f32);
    fn glUniform1f(loc: c_int, x: f32);
    fn glUniform4fv(loc: c_int, count: c_int, value: *const f32);
    fn glUniform1i(loc: c_int, x: c_int);
    fn glGenBuffers(n: c_int, buffers: *mut c_uint);
    fn glBindBuffer(target: c_uint, buffer: c_uint);
    fn glBufferData(target: c_uint, size: isize, data: *const c_void, usage: c_uint);
    fn glEnableVertexAttribArray(index: c_uint);
    fn glVertexAttribPointer(index: c_uint, size: c_int, ty: c_uint, normalized: u8, stride: c_int, pointer: *const c_void);
    fn glDrawArrays(mode: c_uint, first: c_int, count: c_int);
    fn glBindTexture(target: c_uint, texture: c_uint);
    fn glActiveTexture(texture: c_uint);
    fn glEnable(cap: c_uint);
    fn glDisable(cap: c_uint);
    fn glScissor(x: c_int, y: c_int, w: c_int, h: c_int);
    fn glBlendFuncSeparate(src_rgb: c_uint, dst_rgb: c_uint, src_a: c_uint, dst_a: c_uint);
    fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    fn glClear(mask: c_uint);
    #[cfg(feature = "devtriggers")]
    fn glFinish();
    #[cfg(feature = "devtriggers")]
    fn glFlush();
    fn glGenTextures(n: c_int, textures: *mut c_uint);
    fn glDeleteTextures(n: c_int, textures: *const c_uint);
    fn glPixelStorei(pname: c_uint, param: c_int);
    fn glTexImage2D(target: c_uint, level: c_int, ifmt: c_int, w: c_int, h: c_int, border: c_int,
                    format: c_uint, ty: c_uint, pixels: *const c_void);
    fn glTexParameteri(target: c_uint, pname: c_uint, param: c_int);
    // UI self-capture (the "cap_*" section at the bottom of this file)
    fn glGenFramebuffers(n: c_int, ids: *mut c_uint);
    fn glBindFramebuffer(target: c_uint, framebuffer: c_uint);
    fn glFramebufferTexture2D(target: c_uint, attachment: c_uint, textarget: c_uint, texture: c_uint, level: c_int);
    fn glCheckFramebufferStatus(target: c_uint) -> c_uint;
    fn glReadPixels(x: c_int, y: c_int, w: c_int, h: c_int, format: c_uint, ty: c_uint, pixels: *mut c_void);
    fn glCopyTexSubImage2D(target: c_uint, level: c_int, xoff: c_int, yoff: c_int, x: c_int, y: c_int, w: c_int, h: c_int);
    fn glGetError() -> c_uint;
    fn glViewport(x: c_int, y: c_int, w: c_int, h: c_int);
}

const GL_RGBA: c_uint = 0x1908;
const GL_UNSIGNED_BYTE: c_uint = 0x1401;
const GL_UNPACK_ALIGNMENT: c_uint = 0x0CF5;
const GL_TEXTURE_MIN_FILTER: c_uint = 0x2801;
const GL_TEXTURE_MAG_FILTER: c_uint = 0x2800;
const GL_TEXTURE_WRAP_S: c_uint = 0x2802;
const GL_TEXTURE_WRAP_T: c_uint = 0x2803;
const GL_LINEAR: c_int = 0x2601;
const GL_CLAMP_TO_EDGE: c_int = 0x812F;

const GL_COLOR_BUFFER_BIT: c_uint = 0x0000_4000;
const GL_SCISSOR_TEST: c_uint = 0x0C11;

/// Hard-clip subsequent draws to the UI-space rect (top-left origin, 1920×1080 authored coords).
/// Pair with [`clip_clear`]. Scrolling lists use this so a partial row is cut cleanly at the frame
/// edge instead of poking over the video / buttons (`Painter` otherwise has no clip). Rect is
/// clamped to the canvas so a negative edge can't underflow the unsigned scissor box.
///
/// **This is the one drawing call that is NOT in logical coordinates.** Everything else reaches GL
/// through `u_screen`, which the shaders divide by — so the viewport transform scales the whole
/// authored canvas to whatever the drawable is, for free. `glScissor` bypasses all of that: it
/// takes bottom-left *window pixels*. So it needs both the Y flip and the logical→physical scale,
/// which is 1.0 on every television seen so far (`surface::scale`) and would otherwise clip the
/// wrong band of the screen.
/// Where authored coordinates land in the CURRENTLY BOUND target, as the same `(vx, vy, scale)`
/// triple `glViewport` was given, plus the live box a [`clip_clear`] must restore.
///
/// `None` means framebuffer 0, and [`clip_set`] then reads `surface::viewport()`/`scale()` exactly
/// as it always has. It is `Some` only for the duration of [`blur_snapshot_direct`]'s scene draw,
/// which renders the page into a small FBO through a scaled, negative-origin viewport. Without
/// this, every `Painter::clip` inside that draw would compute a full-resolution screen box — the
/// scissor is the one drawing call that is not in logical coordinates — and clip a completely
/// different part of the picture, while `clip_clear`'s bare `glDisable` would additionally throw
/// away the region clamp for the rest of the pass.
static mut CLIP_TARGET: Option<(c_int, c_int, f32, c_int, c_int)> = None;

pub(crate) fn clip_set(x: f32, y: f32, w: f32, h: f32) {
    let x0 = x.max(0.0);
    let y_top = y.max(0.0);
    let x1 = (x + w).min(SCR_W);
    let y1 = (y + h).min(SCR_H);
    // Only the height is needed as an extent now — the x edges are rounded independently and
    // differenced, same as the y ones.
    let hi = (y1 - y_top).max(0.0);
    // The same uniform scale and centring offset `glViewport` was given, because the scissor box
    // has to land on the same pixels the viewport maps to. Deriving both from `surface` rather
    // than duplicating the arithmetic is what keeps them in step.
    let (vx, vy, s) = match unsafe { CLIP_TARGET } {
        Some((tx, ty, ts, _, _)) => (tx, ty, ts),
        None => {
            let (vx, vy, _, _) = crate::surface::viewport();
            (vx, vy, crate::surface::scale())
        }
    };
    // **Round each EDGE in physical space; derive the extent as the difference of the two rounded
    // edges.** Never truncate the origin and the extent independently.
    //
    // The old form did exactly that — four separate `as c_int` — and because
    // `trunc(a) + trunc(b) <= trunc(a + b)`, a box's far edge can fall SHORT of where the next
    // box's near edge begins. Adjacent scissored bands could therefore only ever gap, never abut.
    // With a hard-edged neighbour (`art_scrim`'s gradient quad takes `draw_rect`'s no-AA radius-0
    // fast path, so it paints to an exact boundary) the row in between is covered by nothing and
    // the artwork shows through it — a bright hairline the full width of the tile. Measured in the
    // simulator: one row per seam, ~3x the brightness of its neighbours, its colour tracking the
    // artwork underneath rather than any UI colour.
    //
    // Deriving both edges from the same expression on the same input is what makes it safe: band
    // k's top and band k+1's bottom are then literally the same computation, so float error moves
    // them together and no row can fall between. Rounding is also what the hard-edged quad
    // actually paints (pixel-centre coverage), which truncation structurally cannot match.
    //
    // On a 1:1 surface every value here is already an integer, so this is bit-identical to what
    // the television has always rendered — `snap` rounds in LOGICAL space, and at scale 1.0
    // logical and physical are the same number. That equality is precisely what stopped being
    // true the first time the app met a surface that was not 1920x1080.
    let gx0 = vx + (x0 * s).round() as c_int;
    let gx1 = vx + (x1.max(x0) * s).round() as c_int;
    // GL y is bottom-up, so the box's BOTTOM comes from the band's logical bottom edge.
    let gy0 = vy + ((SCR_H - (y_top + hi)) * s).round() as c_int;
    let gy1 = vy + ((SCR_H - y_top) * s).round() as c_int;
    unsafe {
        glEnable(GL_SCISSOR_TEST);
        glScissor(gx0, gy0, (gx1 - gx0).max(0), (gy1 - gy0).max(0));
    }
}
/// Remove the scissor clip set by [`clip_set`].
///
/// While a render-target override is active this restores that target's LIVE BOX rather than
/// disabling the test: the box is the direct pass's only clamp on where the page may write, and a
/// bare `glDisable` in the middle of the scene draw would let the rest of the page spill across
/// the tap targets' other content.
pub(crate) fn clip_clear() {
    unsafe {
        match CLIP_TARGET {
            Some((_, _, _, tw, th)) => glScissor(0, 0, tw, th),
            None => glDisable(GL_SCISSOR_TEST),
        }
    }
}

/// clear the framebuffer to an opaque color — the retui frame's first op, so the
/// framework doesn't have to link GLES itself (it draws only through gfx/text).
pub(crate) fn frame_clear(r: f32, g: f32, b: f32) {
    unsafe {
        glClearColor(r, g, b, 1.0);
        glClear(GL_COLOR_BUFFER_BIT);
    }
}

/// Block until the GPU has finished all queued commands. Used ONLY as a completion boundary and
/// coarse wall-clock aid for the draw profiler (`ui::profile`) — it is not a GPU timestamp and it
/// serializes the pipeline, so never call it on the normal render path.
#[cfg(feature = "devtriggers")]
pub(crate) fn gl_finish() {
    unsafe { glFinish() }
}

/// Submit everything queued without waiting for it. Unlike [`gl_finish`] this does not stall the
/// CPU, which is what lets the asynchronous timer path use it: on a tile-based Midgard GPU the
/// fragment work for a render target is only issued when that target's pass is flushed, so a
/// `GL_TIME_ELAPSED` interval closed before the flush contains no submitted job and reports a
/// cost of essentially zero. Profiler-only, for the same reason `gl_finish` is.
#[cfg(feature = "devtriggers")]
pub(crate) fn gl_flush() {
    unsafe { glFlush() }
}

static mut PROG: c_uint = 0;
static mut LOC_RECT: c_int = 0;
static mut LOC_SCREEN: c_int = 0;
static mut LOC_SIZE: c_int = 0;
static mut LOC_PAD: c_int = 0;
static mut LOC_RADIUS: c_int = 0;
static mut LOC_COLTOP: c_int = 0;
static mut LOC_COLBOT: c_int = 0;
static mut LOC_FOCUS: c_int = 0;
static mut LOC_FOCUS_RGB: c_int = 0;
static mut LOC_RADR: c_int = 0;
static mut LOC_RIMW: c_int = 0;
static mut LOC_RIMCOL: c_int = 0;

static mut APROG: c_uint = 0;
static mut AL_RECT: c_int = 0;
static mut AL_SCREEN: c_int = 0;
static mut AL_TL: c_int = 0;
static mut AL_TR: c_int = 0;
static mut AL_BR: c_int = 0;
static mut AL_BL: c_int = 0;

static mut SPROG: c_uint = 0;
static mut SL_RECT: c_int = 0;
static mut SL_SCREEN: c_int = 0;
static mut SL_SIZE: c_int = 0;
static mut SL_RADIUS: c_int = 0;
static mut SL_BLUR: c_int = 0;
static mut SL_OFF: c_int = 0;
static mut SL_COL: c_int = 0;

static mut IPROG: c_uint = 0;
static mut IL_RECT: c_int = 0;
static mut IL_SCREEN: c_int = 0;
static mut IL_TINT: c_int = 0;
static mut IL_UVRECT: c_int = 0;
static mut IL_RADIUS: c_int = 0;
static mut IL_TEX: c_int = 0;
static mut IL_RIMW: c_int = 0;
static mut IL_RIMCOL: c_int = 0;
static mut IL_CH: c_int = 0;
static mut IL_SHINV: c_int = 0;
static mut IL_SHCOL: c_int = 0;

/// The `#version` + compatibility preamble prepended to every shader, chosen by the DRIVER's GLSL
/// version rather than by platform.
///
/// The nine sources in `shaders/` are GLSL ES 1.00 — `attribute`, `varying`, `texture2D`,
/// `gl_FragColor`, `precision mediump float;`. That is what the television's GLES2 driver wants,
/// and it needs no preamble there, so this returns an empty string and the sources compile exactly
/// as they always have.
///
/// A desktop core profile has none of those spellings. macOS caps at 4.1 core and has no GLES
/// driver at all, so the simulator gets GLSL 4.10, where the ES names must be macro-mapped. All
/// nine were compiled against a real 4.10 context on an M4 to settle this rather than assume it.
///
/// Two details that are not guesses:
/// - `#define gl_FragColor …` is what works. Declaring `out vec4 gl_FragColor;` is rejected —
///   "Identifier name 'gl_FragColor' cannot start with 'gl_'" — because that restriction binds
///   DECLARATIONS, while the preprocessor substitutes before the parser ever sees the name.
/// - `#line 1` closes the preamble so compiler error line numbers still point at the real source
///   line in `shaders/*.frag`, not at an offset only this function knows.
///
/// Probing the driver rather than keying on `cfg!` means this also answers correctly for a future
/// desktop-GL webOS, a Mesa GLES2 box, or anything else — there is one arm, so nothing can rot.
/// Is this context an OpenGL **ES** driver, asked of the driver once?
///
/// Hoisted out of `glsl_preamble` so the two GL-flavour adaptations cannot disagree. They very
/// nearly did: the preamble probes, while `bind_core_profile_vao` trusted `cfg!(hostsim)` — so a
/// host build against a Mesa GLES2 driver (a Pi, a Linux VM) would have bound a VAO into a context
/// that has none *while* compiling ES shaders. A nonsense state that compiles clean.
fn gl_is_es() -> bool {
    use std::sync::OnceLock;
    static ES: OnceLock<bool> = OnceLock::new();
    *ES.get_or_init(|| unsafe {
        let v = glGetString(GL_SHADING_LANGUAGE_VERSION);
        // "OpenGL ES GLSL ES 1.00" on the television; "4.10" on a desktop core profile. An
        // unreadable string is treated as ES, which is the configuration that needs no preamble
        // and therefore the safe default — a wrong guess there changes nothing.
        // `starts_with`, not a substring scan: the ES spec mandates this exact prefix, while a
        // desktop vendor string containing "ES" anywhere would otherwise skip the preamble and
        // hard-exit at the first shader compile.
        v.is_null() || CStr::from_ptr(v).to_bytes().starts_with(b"OpenGL ES")
    })
}

fn glsl_preamble(ty: c_uint) -> &'static CStr {
    if gl_is_es() {
        return c"";
    }
    if ty == GL_VERTEX_SHADER {
        c"#version 410 core\n#define attribute in\n#define varying out\n#define texture2D texture\n#line 1\n"
    } else {
        c"#version 410 core\n#define varying in\n#define texture2D texture\nout vec4 plx_frag;\n#define gl_FragColor plx_frag\n#line 1\n"
    }
}

pub(crate) fn gfx_compile(ty: c_uint, src: *const c_char) -> c_uint {
    unsafe {
        let s = glCreateShader(ty);
        // Two source strings rather than a concatenation: GL joins them itself, so the preamble
        // needs no allocation and the original `&CStr` sources stay untouched.
        let srcs: [*const c_char; 2] = [glsl_preamble(ty).as_ptr(), src];
        glShaderSource(s, 2, srcs.as_ptr(), std::ptr::null());
        glCompileShader(s);
        let mut ok: c_int = 0;
        glGetShaderiv(s, GL_COMPILE_STATUS, &mut ok);
        if ok == 0 {
            let mut buf = [0u8; 1024];
            glGetShaderInfoLog(s, 1024, std::ptr::null_mut(), buf.as_mut_ptr() as *mut c_char);
            let msg = CStr::from_ptr(buf.as_ptr() as *const c_char).to_string_lossy().into_owned();
            // The EVENT log is the only surface anyone reads over ssh, and neither of the two lines
            // below reaches it: `eprintln!` goes to /tmp/plxnative-stderr.log (main.c replaces
            // stderr), and `process::exit` is a CLEAN exit, so the crash tracer never fires and
            // /tmp/plxnative-crash.log stays empty. Without this the window flashes, the app is back
            // at the launcher, the event log simply stops mid-boot, and triage correctly reports
            // "not a crash" with nothing pointing at the shader. `eprintln!` stays because it costs
            // nothing, NOT because it is the durable copy: `main.c` truncates both sinks at every
            // launch, so neither survives the relaunch that `plxnative-crash.log` is append-only for.
            log(&format!("shader compile FAILED — exiting: {msg}"));
            eprintln!("shader error: {msg}");
            std::process::exit(1);
        }
        s
    }
}

/// Shared program bring-up for every shader pair: create → attach VS/FS → bind `a_pos`
/// (attrib 0, the shared unit quad) → link. `None` = link failure; each caller keeps its
/// own failure policy (hard-exit, degrade to 0, or early-return).
pub(crate) fn link_program(vs: *const c_char, fs: *const c_char) -> Option<c_uint> {
    unsafe {
        let p = glCreateProgram();
        glAttachShader(p, gfx_compile(GL_VERTEX_SHADER, vs));
        glAttachShader(p, gfx_compile(GL_FRAGMENT_SHADER, fs));
        glBindAttribLocation(p, 0, c"a_pos".as_ptr());
        glLinkProgram(p);
        let mut ok: c_int = 0;
        glGetProgramiv(p, GL_LINK_STATUS, &mut ok);
        (ok != 0).then_some(p)
    }
}

/// Lazy program binding: the driver call is skipped when `p` is already current. Every draw fn
/// binds ITS OWN program through this (draw_rect no longer assumes PROG is left bound by whoever
/// ran before it), which deletes the unconditional switch+restore pair the textured/text/ambient/
/// shadow paths used to pay on every call. Main-thread only, like all GL here.
static mut CUR_PROG: c_uint = 0;
#[inline]
pub(crate) fn use_prog(p: c_uint) {
    unsafe {
        if CUR_PROG != p {
            glUseProgram(p);
            CUR_PROG = p;
        }
    }
}

static QUAD: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];

/// Bind the one vertex array object a desktop core profile requires.
///
/// **GLES2 has no VAOs and a core profile has no DEFAULT one**, and the difference is silent: with
/// VAO 0 bound, `glVertexAttribPointer` and `glDrawArrays` raise `GL_INVALID_OPERATION` and draw
/// nothing at all. `glClear` is unaffected, so the symptom is a window painted the clear colour and
/// a completely empty interface — which is exactly what the first simulator screenshots were, at
/// every frame sampled.
///
/// One VAO for the process is enough and is not a simplification: this renderer has a single
/// attribute layout — attribute 0, the shared unit quad in one static VBO, set up immediately
/// below and never rebound. There is nothing for a second VAO to describe.
///
/// `#[cfg]` rather than a runtime check because these two entry points do not EXIST in GLES2; the
/// television's libGLESv2 exports neither, so merely naming them in the shared `extern` block
/// would be a link-time undefined symbol on the device.
#[cfg(feature = "hostsim")]
fn bind_core_profile_vao() {
    extern "C" {
        fn glGenVertexArrays(n: c_int, arrays: *mut c_uint);
        fn glBindVertexArray(array: c_uint);
    }
    // A hostsim build is not necessarily a core profile — Mesa GLES2 exists on desktops too, and
    // there VAO 0 is legal and these entry points are absent. Ask the driver, like the preamble.
    if gl_is_es() {
        return;
    }
    unsafe {
        let mut vao: c_uint = 0;
        glGenVertexArrays(1, &mut vao);
        glBindVertexArray(vao);
    }
}

pub(crate) fn init_gl() {
    unsafe {
        // Before any buffer or attribute state is touched — in a core profile the calls below are
        // errors without it.
        #[cfg(feature = "hostsim")]
        bind_core_profile_vao();

        PROG = link_program(VS_SRC.as_ptr(), FS_SRC.as_ptr()).unwrap_or_else(|| {
            // Logged for the same reason as the compile failure in `gfx_compile`: stderr is not the
            // event log and a clean exit is not a crash, so this line is the only trace the death
            // leaves. The base program is the one every draw needs, hence the exit where the
            // ambient/shadow/image programs below merely degrade.
            log("base prog link FAILED — exiting");
            eprintln!("link failed");
            std::process::exit(1);
        });
        use_prog(PROG);
        LOC_RECT = glGetUniformLocation(PROG, c"u_rect".as_ptr());
        LOC_SCREEN = glGetUniformLocation(PROG, c"u_screen".as_ptr());
        LOC_SIZE = glGetUniformLocation(PROG, c"u_size".as_ptr());
        LOC_PAD = glGetUniformLocation(PROG, c"u_pad".as_ptr());
        LOC_RADIUS = glGetUniformLocation(PROG, c"u_radius".as_ptr());
        LOC_COLTOP = glGetUniformLocation(PROG, c"u_colTop".as_ptr());
        LOC_COLBOT = glGetUniformLocation(PROG, c"u_colBot".as_ptr());
        LOC_FOCUS = glGetUniformLocation(PROG, c"u_focus".as_ptr());
        LOC_FOCUS_RGB = glGetUniformLocation(PROG, c"u_focus_rgb".as_ptr());
        LOC_RADR = glGetUniformLocation(PROG, c"u_radR".as_ptr());
        LOC_RIMW = glGetUniformLocation(PROG, c"u_rimw".as_ptr());
        LOC_RIMCOL = glGetUniformLocation(PROG, c"u_rimcol".as_ptr());
        glUniform2f(LOC_SCREEN, SCR_W, SCR_H);

        let mut vbo: c_uint = 0;
        glGenBuffers(1, &mut vbo);
        glBindBuffer(GL_ARRAY_BUFFER, vbo);
        glBufferData(GL_ARRAY_BUFFER, std::mem::size_of_val(&QUAD) as isize, QUAD.as_ptr() as *const c_void, GL_STATIC_DRAW);
        glEnableVertexAttribArray(0);
        glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, std::ptr::null());

        APROG = link_program(VS_SRC.as_ptr(), FS_AMBIENT.as_ptr()).unwrap_or_else(|| {
            log("ambient prog link failed");
            0 // draw_ambient then binds program 0 and draws nothing — the corner wash is a nicety
        });
        if APROG != 0 {
            AL_RECT = glGetUniformLocation(APROG, c"u_rect".as_ptr());
            AL_SCREEN = glGetUniformLocation(APROG, c"u_screen".as_ptr());
            AL_TL = glGetUniformLocation(APROG, c"u_atl".as_ptr());
            AL_TR = glGetUniformLocation(APROG, c"u_atr".as_ptr());
            AL_BR = glGetUniformLocation(APROG, c"u_abr".as_ptr());
            AL_BL = glGetUniformLocation(APROG, c"u_abl".as_ptr());
        }

        // Soft-shadow program (own program so the hot FS_SRC pays nothing; mirrors init_image).
        SPROG = link_program(VS_SRC.as_ptr(), FS_SHADOW.as_ptr()).unwrap_or_else(|| {
            log("shadow prog link failed");
            0 // draw_shadow no-ops → cards simply lose the drop-shadow, nothing else breaks
        });
        if SPROG != 0 {
            SL_RECT = glGetUniformLocation(SPROG, c"u_rect".as_ptr());
            SL_SCREEN = glGetUniformLocation(SPROG, c"u_screen".as_ptr());
            SL_SIZE = glGetUniformLocation(SPROG, c"u_size".as_ptr());
            SL_RADIUS = glGetUniformLocation(SPROG, c"u_radius".as_ptr());
            SL_BLUR = glGetUniformLocation(SPROG, c"u_blur".as_ptr());
            SL_OFF = glGetUniformLocation(SPROG, c"u_off".as_ptr());
            SL_COL = glGetUniformLocation(SPROG, c"u_col".as_ptr());
        }

        // Hoist the compile-time-constant uniforms: uniforms are per-program state, so each
        // program's screen size is set ONCE here instead of on every draw call. (The UI is a
        // fixed 1920x1080 with no DPI scaling — that constancy is what makes this legal.)
        if APROG != 0 {
            use_prog(APROG);
            glUniform2f(AL_SCREEN, SCR_W, SCR_H);
        }
        if SPROG != 0 {
            use_prog(SPROG);
            glUniform2f(SL_SCREEN, SCR_W, SCR_H);
        }
        use_prog(PROG);
        // Texture unit 0 is the only unit this renderer ever samples from — set it once.
        glActiveTexture(GL_TEXTURE0);

        glEnable(GL_BLEND);
        // **SEPARATE, and the alpha half is the whole point.** Colour composites the ordinary way;
        // ALPHA must ACCUMULATE (`GL_ONE`), because this surface is a wayland surface the compositor
        // blends over the video plane — `system.rs` deliberately makes it non-opaque — so the alpha
        // channel is not scratch space, it is what the television multiplies our picture by.
        //
        // With one `glBlendFunc` for both, alpha used `GL_SRC_ALPHA` as well and every partial-alpha
        // draw over an opaque ground computed `dst.a = a² + dst.a(1−a)`: at a=.40 the surface fell to
        // **0.76** where it had been 1. The colour was right and the SURFACE had a hole in it, and
        // the compositor showed black through the hole — measured on the panel as 43 → 33, which is
        // exactly `43 × 0.76`.
        //
        // It is near-invisible in a screenshot and a hard bar on the television, because sRGB 43 vs
        // 33 is ~2× in luminance down there. Found through the search screen's navigation-bar scrim
        // (reported as "on screenshot capture I see a pleasing fade, but on TV it is like a shadow"),
        // but it was never that screen's bug: EVERY partial-alpha draw in this app punched the same
        // hole — the Library's scrim, both hero scrims, `art_scrim`, every popover scrim, every page
        // and content fade, every glyph drawn at less than full alpha. Fades have always come out a
        // shade darker than authored on the panel and nothing but a photograph could show it.
        //
        // The player's transparent clear is unaffected in the right direction: over `dst.a = 0`,
        // accumulating alpha gives `src.a`, which is what a HUD drawn over the video plane means.
        glBlendFuncSeparate(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA, GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
        // GL_DITHER is ON by default in GLES2; it dithers low-alpha gradients (the card shadow
        // penumbra) into a regular ordered-dither dot pattern visible along tile edges. The panel is
        // 888 and SURFACE_APP is snapped to exact 8-bit codes, so dithering buys nothing here — off.
        glDisable(GL_DITHER);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    pad: f32,
    radius: f32,
    top: *const f32,
    bot: *const f32,
    focus: f32,
    focus_rgb: f32,
) {
    if culled(x, y, w, h) {
        return;
    }
    unsafe {
        use_prog(PROG);
        // Only the rounded/focus SDF path needs the AA bleed; a plain rect takes the fast-path fill
        // and must stay exactly its bounds (a 1px overhang would fatten scrims/backgrounds).
        let aa = if radius >= 0.5 || focus > 0.001 { AA_BLEED } else { 0.0 };
        glUniform4f(LOC_RECT, x - aa, y - aa, w + 2.0 * aa, h + 2.0 * aa);
        glUniform2f(LOC_SIZE, w + 2.0 * aa, h + 2.0 * aa);
        glUniform1f(LOC_PAD, pad + aa);
        glUniform1f(LOC_RADIUS, radius);
        glUniform1f(LOC_RADR, radius);
        glUniform4fv(LOC_COLTOP, 1, top);
        glUniform4fv(LOC_COLBOT, 1, bot);
        glUniform1f(LOC_FOCUS, focus);
        // Fill/rim colours arrive pre-scaled by Painter::rgb. The focus ring/glow is generated in
        // the shader, so it needs the same RGB gain explicitly while its alpha coverage stays put.
        if focus > 0.001 {
            glUniform1f(LOC_FOCUS_RGB, focus_rgb);
        }
        glUniform1f(LOC_RIMW, 0.0);
        glUniform4f(LOC_RIMCOL, 0.0, 0.0, 0.0, 0.0); // no edge-sheen (default)
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
}

/// [`draw_rect`] with the focus edge-sheen (a `rimw`-px inset perimeter rim in `rimcol`) baked into
/// the same fill pass — the no-texture (skeleton / chip disc) counterpart of [`draw_tex_stroked`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_rect_sheened(x: f32, y: f32, w: f32, h: f32, radius: f32, top: *const f32, bot: *const f32, rimw: f32, rimcol: *const f32) {
    if culled(x, y, w, h) {
        return;
    }
    unsafe {
        use_prog(PROG);
        let aa = AA_BLEED; // a sheened tile is always rounded → always SDF path
        glUniform4f(LOC_RECT, x - aa, y - aa, w + 2.0 * aa, h + 2.0 * aa);
        glUniform2f(LOC_SIZE, w + 2.0 * aa, h + 2.0 * aa);
        glUniform1f(LOC_PAD, aa);
        glUniform1f(LOC_RADIUS, radius);
        glUniform1f(LOC_RADR, radius);
        glUniform4fv(LOC_COLTOP, 1, top);
        glUniform4fv(LOC_COLBOT, 1, bot);
        glUniform1f(LOC_FOCUS, 0.0);
        glUniform1f(LOC_RIMW, rimw);
        glUniform4fv(LOC_RIMCOL, 1, rimcol);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
}

/// The OPAQUE 4-corner gradient: rgb corners scaled by `dim`, alpha forced to 1.0. That literal
/// `1.0` is **load-bearing**, not incidental — `fs_ambient.frag` now interpolates alpha with the
/// colour (see [`draw_grad4`]), so this is what keeps every ambient wash a ground that REPLACES what
/// is under it rather than a translucent film over it.
pub(crate) fn draw_ambient(x: f32, y: f32, w: f32, h: f32, dim: f32, tl: *const f32, tr: *const f32, br: *const f32, bl: *const f32) {
    if culled(x, y, w, h) {
        return;
    }
    unsafe {
        let c3 = |p: *const f32, i: usize| *p.add(i);
        use_prog(APROG); // AL_SCREEN is set once at init (uniforms are per-program state)
        glUniform4f(AL_RECT, x, y, w, h);
        glUniform4f(AL_TL, c3(tl, 0) * dim, c3(tl, 1) * dim, c3(tl, 2) * dim, 1.0);
        glUniform4f(AL_TR, c3(tr, 0) * dim, c3(tr, 1) * dim, c3(tr, 2) * dim, 1.0);
        glUniform4f(AL_BR, c3(br, 0) * dim, c3(br, 1) * dim, c3(br, 2) * dim, 1.0);
        glUniform4f(AL_BL, c3(bl, 0) * dim, c3(bl, 1) * dim, c3(bl, 2) * dim, 1.0);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
}

/// The 4-corner bilinear gradient with REAL per-corner ALPHA (rgba each) — the hero corner scrim's
/// primitive, and the reason `fs_ambient.frag` mixes vec4 instead of vec3. [`draw_ambient`] is this
/// with every corner forced opaque; the two share one program because they are one field.
///
/// Each pointer must address FOUR floats (rgba). Blending is the app-wide
/// `GL_SRC_ALPHA`/`GL_ONE_MINUS_SRC_ALPHA` set at init, so this composites over whatever is already
/// on the panel — which is the whole point of having it beside the opaque wash.
pub(crate) fn draw_grad4(x: f32, y: f32, w: f32, h: f32, tl: *const f32, tr: *const f32, br: *const f32, bl: *const f32) {
    if culled(x, y, w, h) {
        return;
    }
    unsafe {
        use_prog(APROG); // AL_SCREEN is set once at init (uniforms are per-program state)
        glUniform4f(AL_RECT, x, y, w, h);
        glUniform4fv(AL_TL, 1, tl);
        glUniform4fv(AL_TR, 1, tr);
        glUniform4fv(AL_BR, 1, br);
        glUniform4fv(AL_BL, 1, bl);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
}

pub(crate) fn draw_rrect(x: f32, y: f32, w: f32, h: f32, rad_l: f32, rad_r: f32, col: *const f32) {
    if culled(x, y, w, h) {
        return;
    }
    unsafe {
        use_prog(PROG);
        // Rounded corners always take the SDF path, so always give the edge band its bleed.
        let aa = if rad_l >= 0.5 || rad_r >= 0.5 { AA_BLEED } else { 0.0 };
        glUniform4f(LOC_RECT, x - aa, y - aa, w + 2.0 * aa, h + 2.0 * aa);
        glUniform2f(LOC_SIZE, w + 2.0 * aa, h + 2.0 * aa);
        glUniform1f(LOC_PAD, aa);
        glUniform1f(LOC_RADIUS, rad_l);
        glUniform1f(LOC_RADR, rad_r);
        glUniform4fv(LOC_COLTOP, 1, col);
        glUniform4fv(LOC_COLBOT, 1, col);
        glUniform1f(LOC_FOCUS, 0.0);
        glUniform1f(LOC_RIMW, 0.0);
        glUniform4f(LOC_RIMCOL, 0.0, 0.0, 0.0, 0.0); // no edge-sheen (default)
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
}

/// [`draw_rrect`] with the focus edge-sheen baked in (flat fill + `rimw`-px inset rim in `rimcol`).
pub(crate) fn draw_rrect_sheened(x: f32, y: f32, w: f32, h: f32, rad_l: f32, rad_r: f32, col: *const f32, rimw: f32, rimcol: *const f32) {
    if culled(x, y, w, h) {
        return;
    }
    unsafe {
        use_prog(PROG);
        let aa = AA_BLEED;
        glUniform4f(LOC_RECT, x - aa, y - aa, w + 2.0 * aa, h + 2.0 * aa);
        glUniform2f(LOC_SIZE, w + 2.0 * aa, h + 2.0 * aa);
        glUniform1f(LOC_PAD, aa);
        glUniform1f(LOC_RADIUS, rad_l);
        glUniform1f(LOC_RADR, rad_r);
        glUniform4fv(LOC_COLTOP, 1, col);
        glUniform4fv(LOC_COLBOT, 1, col);
        glUniform1f(LOC_FOCUS, 0.0);
        glUniform1f(LOC_RIMW, rimw);
        glUniform4fv(LOC_RIMCOL, 1, rimcol);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
}

/// Soft drop-shadow of the box `(x,y,w,h)` with corner `radius` (w/2 = circle), penumbra `blur` px.
/// The quad is inflated by `blur` on every side; the shader falls the alpha off outward over that
/// band (see `FS_SHADOW`). `(x,y)` is the shadow's box origin — the caller bakes any downward offset
/// into `y`. No-ops if the program failed to link. Own GL program (bound lazily via [`use_prog`]),
/// so it doesn't disturb the base shader's uniforms.
pub(crate) fn draw_shadow(x: f32, y: f32, w: f32, h: f32, radius: f32, blur: f32, off: f32, col: *const f32) {
    if culled(x, y, w, h) {
        return;
    }
    unsafe {
        if SPROG == 0 {
            return;
        }
        let b = blur.max(0.5);
        let (qx, qy, qw, qh) = (x - b, y - b, w + 2.0 * b, h + 2.0 * b);
        use_prog(SPROG); // SL_SCREEN is set once at init
        glUniform4f(SL_RECT, qx, qy, qw, qh);
        glUniform2f(SL_SIZE, qw, qh);
        glUniform1f(SL_RADIUS, radius);
        glUniform1f(SL_BLUR, b);
        glUniform1f(SL_OFF, off); // occluder (tile) offset above the shadow box; shader discards the covered interior
        glUniform4fv(SL_COL, 1, col);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
}

/// Critically-damped spring step — the **exact analytic** solution of `x'' + 2ω·x' + ω²·x = 0`
/// (ω = √k, offset `x = pos − target`) integrated over `dt`.
///
/// Because it is the closed form, it is unconditionally stable at any `dt` *and* never overshoots
/// (critical damping does not ring, by definition). It replaces two flawed discretizations:
/// - explicit Euler (`vel += (k·x − c·vel)·dt`) had transition-matrix `det = 1 − 2√k·dt`, so at the
///   clamped `dt = 0.05` any spring with `k ≳ 275` grew every frame and exploded — a k=300 focus-pop
///   reached a scale of 5·10⁵ (the "scatter"); and
/// - implicit-damped Euler stayed bounded but was underdamped at large `dt`, so it rang (~4%
///   overshoot — a visible "bounce").
///
/// `x(t) = (x₀ + (v₀ + ω·x₀)·t)·e^(−ω·t)`, and its derivative for the velocity.
pub(crate) fn spring(pos: *mut f32, vel: *mut f32, target: f32, k: f32, dt: f32) {
    unsafe {
        let w = k.sqrt(); // natural frequency; critical damping is c = 2ω
        let e = (-w * dt).exp();
        let x = *pos - target; // offset from target
        let b = *vel + w * x;
        *pos = target + (x + b * dt) * e;
        *vel = (*vel - w * b * dt) * e;
        // Every animation in the app lands here or in `spring_zeta`, which is what lets
        // `ui::idle` know EXACTLY whether the screen is still moving without any screen opting in.
        crate::ui::idle::note_spring(*pos, target, *vel);
    }
}

/// **Under**damped spring step — the closed-form solution of `x'' + 2ζω·x' + ω²·x = 0` (ω = √k) for a
/// damping ratio `zeta < 1`, so unlike [`spring`] (ζ = 1, critical, never overshoots) it **rings**:
/// it swings past the target and settles back. That overshoot is the tvOS "click" pop — the press
/// spring-back in `ui::press` uses it (ζ ≈ 0.55) so a released card bounces a hair past its focus
/// scale before resting. Like [`spring`] it is the exact analytic form, so it is unconditionally
/// stable at any `dt` (the envelope `e^(−ζω·dt)` only ever decays). The focus-pop / scroll springs
/// stay on [`spring`] — they must NOT ring.
///
/// `x(t) = e^(−ζω·t)·(A·cos(ω_d·t) + B·sin(ω_d·t))`, ω_d = ω·√(1−ζ²), A = x₀, B = (v₀ + ζω·x₀)/ω_d.
pub(crate) fn spring_zeta(pos: *mut f32, vel: *mut f32, target: f32, k: f32, zeta: f32, dt: f32) {
    unsafe {
        let w = k.sqrt();
        let z = zeta.clamp(0.0, 0.999); // guard the ω_d = 0 singularity at critical/over-damping
        let wd = w * (1.0 - z * z).sqrt(); // damped natural frequency
        let x0 = *pos - target; // offset from target
        let v0 = *vel;
        let e = (-z * w * dt).exp();
        let (s, c) = (wd * dt).sin_cos();
        let a = x0;
        let b = (v0 + z * w * x0) / wd;
        *pos = target + e * (a * c + b * s);
        *vel = e * ((b * wd - z * w * a) * c - (a * wd + z * w * b) * s);
        crate::ui::idle::note_spring(*pos, target, *vel); // see the note in `spring`
    }
}

// --- seven-segment FPS digits (quads) ---
//
// The counter's ONLY caller is `app.rs`'s `#[cfg(feature = "devtools")]` draw site, so these three
// items carry the same gate. Without it `--no-default-features` (i.e. `make RELEASE=1`, and the
// macOS app bundle) fails the build outright rather than merely warning: `[lints.rust] warnings =
// "deny"` in Cargo.toml turns the three `dead_code` findings into errors, and nothing in the dev
// configuration can see that — the feature is on in every ordinary `make`, `make check` and
// harness run.
#[cfg(feature = "devtools")]
const SEG: [u8; 10] = [0x3F, 0x06, 0x5B, 0x4F, 0x66, 0x6D, 0x7D, 0x07, 0x7F, 0x6F];

#[cfg(feature = "devtools")]
fn draw_digit(d: i32, x: f32, y: f32, s: f32, col: *const f32) {
    let w = 0.16 * s;
    // segments: 0 top,1 tr,2 br,3 bottom,4 bl,5 tl,6 mid — each {x,y,w,h}
    let g: [[f32; 4]; 7] = [
        [0.0, 0.0, 1.0, 0.0],
        [1.0, 0.0, 0.0, 0.5],
        [1.0, 0.5, 0.0, 0.5],
        [0.0, 1.0, 1.0, 0.0],
        [0.0, 0.5, 0.0, 0.5],
        [0.0, 0.0, 0.0, 0.5],
        [0.0, 0.5, 1.0, 0.0],
    ];
    for i in 0..7 {
        if (SEG[d as usize] >> i) & 1 == 0 {
            continue;
        }
        let sx = x + g[i][0] * s - w / 2.0;
        let sy = y + g[i][1] * s - w / 2.0;
        let sw = g[i][2] * s + w;
        let sh = g[i][3] * s + w;
        draw_rect(sx, sy, sw, sh, 2.0, (w + 4.0) / 2.0 - 2.0, col, col, 0.0, 1.0);
    }
}

#[cfg(feature = "devtools")]
pub(crate) fn draw_number(mut n: i32, right_x: f32, y: f32, s: f32, col: *const f32) {
    n = n.clamp(0, 999);
    let adv = s + 0.55 * s;
    let mut x = right_x - adv;
    loop {
        draw_digit(n % 10, x, y, s, col);
        n /= 10;
        x -= adv;
        if n <= 0 {
            break;
        }
    }
}

// ---- image program: RGBA textures (posters/logos/backdrop) with rounded corners ----
pub(crate) fn init_image() {
    unsafe {
        IPROG = match link_program(VS_IMG.as_ptr(), FS_IMG.as_ptr()) {
            Some(p) => p,
            None => {
                log("image prog link failed");
                return;
            }
        };
        IL_RECT = glGetUniformLocation(IPROG, c"u_trect".as_ptr());
        IL_SCREEN = glGetUniformLocation(IPROG, c"u_tscreen".as_ptr());
        IL_TINT = glGetUniformLocation(IPROG, c"u_tint".as_ptr());
        IL_UVRECT = glGetUniformLocation(IPROG, c"u_uvrect".as_ptr());
        IL_RADIUS = glGetUniformLocation(IPROG, c"u_iradius".as_ptr());
        IL_TEX = glGetUniformLocation(IPROG, c"u_tex".as_ptr());
        IL_RIMW = glGetUniformLocation(IPROG, c"u_rimw".as_ptr());
        IL_RIMCOL = glGetUniformLocation(IPROG, c"u_rimcol".as_ptr());
        IL_CH = glGetUniformLocation(IPROG, c"u_ch".as_ptr());
        IL_SHINV = glGetUniformLocation(IPROG, c"u_shinv".as_ptr());
        IL_SHCOL = glGetUniformLocation(IPROG, c"u_shcol".as_ptr());
        // Set this program's constant uniforms once (per-program state): the fixed screen size
        // and sampler unit 0. draw_tex_impl no longer re-sends them per quad.
        use_prog(IPROG);
        glUniform2f(IL_SCREEN, SCR_W, SCR_H);
        glUniform1i(IL_TEX, 0);
        use_prog(PROG);
    }
}

/// Snap a composited quad origin to a whole pixel — the contract for ALL 1:1-texel content
/// (glyph strings in `text.rs`, icon masks in `ui/icons.rs`): such textures are rasterized at
/// their exact draw size and sampled with GL_LINEAR, so a fractional origin bilinear-smears
/// every texel across two pixels (washed glyph stems, fuzzy icon edges). Snap the FINAL
/// composited position (after any Painter translate fold), and never apply this to scaled
/// content — posters and animating quads legitimately move sub-pixel.
#[inline]
pub(crate) fn snap(v: f32) -> f32 {
    v.round()
}

/// Upload a straight-alpha RGBA8 bitmap (`w`×`h`, tightly packed) into a GL texture. Reuses
/// `prev` if non-zero (re-specs it), else allocates a new id. Returns the texture id. Used for
/// image-subtitle (PGS/VobSub) overlays and the text glyph-cache textures. Main-thread only.
pub(crate) fn upload_rgba(prev: c_uint, w: c_int, h: c_int, pixels: *const u8) -> c_uint {
    unsafe {
        let mut tex = prev;
        if tex == 0 {
            glGenTextures(1, &mut tex);
        }
        glBindTexture(GL_TEXTURE_2D, tex);
        glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA as c_int, w, h, 0, GL_RGBA, GL_UNSIGNED_BYTE, pixels as *const c_void);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        tex
    }
}

/// Delete a texture created by upload_rgba (0 = no-op). Main-thread only.
pub(crate) fn delete_tex(tex: c_uint) {
    if tex != 0 {
        unsafe { glDeleteTextures(1, &tex) };
    }
}

/// The card-composite draw. `(x,y,w,h)` is the CARD rect; the quad is inflated by `pad` so the shadow
/// penumbra fits, and `FS_IMG` remaps the texture back to the card. `rimw`/`rimcol` = the 1px edge
/// sheen; `pad`/`shblur`/`shcol` = the soft (symmetric) drop-shadow (all zero ⇒ a plain rounded texture).
/// The UV sub-rect `(offset.xy, scale.zw)` for a quad inflated by `pad` around a `w`×`h` card —
/// i.e. "map the texture back onto the card, not onto the shadow ring".
///
/// Pure, and split out because it is the identity `vs_img.vert` used to hard-code as a scale about
/// 0.5: `(a_pos - 0.5) * s + 0.5` is `a_pos * s + (0.5 - 0.5 * s)`. Keeping the algebra here (with
/// a test) is what let the vertex shader take a general offset for [`draw_blur_backdrop`] without
/// anyone having to re-derive the card path's numbers.
#[inline]
fn uv_rect_padded(w: f32, h: f32, qw: f32, qh: f32) -> [f32; 4] {
    let sx = if w > 0.0 { qw / w } else { 1.0 };
    let sy = if h > 0.0 { qh / h } else { 1.0 };
    [0.5 - 0.5 * sx, 0.5 - 0.5 * sy, sx, sy]
}

/// The IPROG draw, with every term already in the shader's own units: `q*` is the QUAD (shadow
/// inflation included), `uv` the source sub-rect it samples, `ch` the CARD half-size the SDF is
/// measured against. [`draw_tex_impl`] folds a card's parameters into these; the blur backdrop
/// supplies its own, because its UV window is a screen-space rect rather than a card.
#[allow(clippy::too_many_arguments)]
fn draw_tex_core(tex: c_uint, qx: f32, qy: f32, qw: f32, qh: f32, uv: [f32; 4], radius: f32, tint: *const f32,
    rimw: f32, rimcol: *const f32, chw: f32, chh: f32, shinv: f32, shcol: *const f32) {
    if tex == 0 || culled(qx, qy, qw, qh) {
        return;
    }
    unsafe {
        use_prog(IPROG); // IL_SCREEN / IL_TEX / texture unit 0 are set once at init
        glUniform4fv(IL_TINT, 1, tint);
        glUniform4f(IL_UVRECT, uv[0], uv[1], uv[2], uv[3]);
        glUniform1f(IL_RADIUS, radius);
        glUniform1f(IL_RIMW, rimw);
        glUniform4fv(IL_RIMCOL, 1, rimcol);
        glUniform2f(IL_CH, chw, chh);
        glUniform1f(IL_SHINV, shinv);
        glUniform4fv(IL_SHCOL, 1, shcol);
        glBindTexture(GL_TEXTURE_2D, tex);
        glUniform4f(IL_RECT, qx, qy, qw, qh);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_tex_impl(tex: c_uint, x: f32, y: f32, w: f32, h: f32, radius: f32, tint: *const f32, rimw: f32, rimcol: *const f32,
    pad: f32, shblur: f32, shcol: *const f32) {
    let (qx, qy, qw, qh) = (x - pad, y - pad, w + 2.0 * pad, h + 2.0 * pad); // inflate for the penumbra
    // CPU-fold the uniform-only terms (Midgard has no uniform pre-shader): card half-size, the
    // quad→card UV rect (identity when pad==0), and the shadow's 0.5/blur normaliser.
    let uv = uv_rect_padded(w, h, qw, qh);
    let shinv = if shblur > 0.0 { 0.5 / shblur } else { 0.0 };
    draw_tex_core(tex, qx, qy, qw, qh, uv, radius, tint, rimw, rimcol, w * 0.5, h * 0.5, shinv, shcol);
}

const NO_RIM: [f32; 4] = [0.0, 0.0, 0.0, 0.0]; // rim/shadow disabled: alpha 0 ⇒ shader skips it

pub(crate) fn draw_tex(tex: c_uint, x: f32, y: f32, w: f32, h: f32, radius: f32, tint: *const f32) {
    draw_tex_impl(tex, x, y, w, h, radius, tint, 0.0, NO_RIM.as_ptr(), 0.0, 0.0, NO_RIM.as_ptr());
}

/// [`draw_tex`] plus the focus edge-sheen baked into the same pass (rim only, no shadow). Used for the
/// profile chip avatar.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_tex_stroked(tex: c_uint, x: f32, y: f32, w: f32, h: f32, radius: f32, tint: *const f32, rimw: f32, rimcol: *const f32) {
    draw_tex_impl(tex, x, y, w, h, radius, tint, rimw, rimcol, 0.0, 0.0, NO_RIM.as_ptr());
}

/// The full card composite: texture + edge sheen (`rimw`/`rimcol`) + soft symmetric drop-shadow
/// (`pad`/`shblur`/`shcol`), one pass. Used for every art tile (posters, episode stills, cast/profile
/// circles) so the resting-and-rising shadow costs only the inflation ring, not a separate pass.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_tex_carded(tex: c_uint, x: f32, y: f32, w: f32, h: f32, radius: f32, tint: *const f32,
    rimw: f32, rimcol: *const f32, pad: f32, shblur: f32, shcol: *const f32) {
    // Not during a blur source pass: these are read once a frame by the framedrop tool as "cards
    // the panel composited", and a second page draw would report twice the real number.
    if !blur_source_pass() {
        CARD_CT.fetch_add(1, Ordering::Relaxed);
    }
    // the inflated (shadow) quad crossing a screen edge ⇒ some shadow fragments are drawn off-screen
    // (viewport-clipped, but still rasterized). Counts partial+full; fully-off-screen ⇒ a cull miss.
    if x - pad < 0.0 || y - pad < 0.0 || x + w + pad > SCR_W || y + h + pad > SCR_H {
        if !blur_source_pass() {
            CARD_OFF.fetch_add(1, Ordering::Relaxed);
        }
    }
    draw_tex_impl(tex, x, y, w, h, radius, tint, rimw, rimcol, pad, shblur, shcol);
}

use crate::log;

// ============================== backdrop blur ================================
// The frosted ground under a popover panel: a blurred snapshot of what the frame had drawn BEHIND
// it, sampled through the panel's own rounded rect. `track_menu.rs` carried "no true backdrop blur
// on the GLES plane" as a standing assumption for a year; this is what replaces it.
//
// Four facts shape the whole design.
//
// 1. **Snapshots are cached.** A popover over a still page captures on open and then costs one
//    textured quad per drawn frame. A moving opaque UI page may opt into `widgets::Glass::DYNAMIC`,
//    which invalidates at most every third successful present while its underlay is dirty; the
//    widget still draws every present.
//    Capturing every present was measured at 52.6 fps on the dev television and is not supported.
// 2. **The capture is MID-FRAME.** `Painter`'s primitives are immediate GL calls, so the default
//    framebuffer already holds exactly the prepared page (source-dimmed, or with an overlay scrim)
//    at the moment the panel is about to draw.
//    That is the only reason no render-target restructuring is needed: the "background" is a
//    definition of *when*, not of *what*.
//    **A tested and REJECTED hypothesis lives here**, recorded because it is the obvious next idea
//    and it is wrong on this part: that a mid-frame framebuffer read is expensive *because it is
//    mid-frame* — a tiler must resolve the frame's deferred passes before the copy can see them,
//    so moving the snapshot beside `capture::tick` (after the last draw, where the swap pays that
//    flush anyway) should have been free. Built and measured on the dev set: **48.1 ms deferred vs
//    45.3 ms mid-frame** — no difference, so the position costs nothing and the simpler code is the
//    one to keep. The cost is elsewhere; see the measurements on [`blur_snapshot`].
// 3. **Passes are a large part of the budget.** Two exact-2x reductions, two quarter-size Kawase
//    passes and one half-size up-filter make five render passes. Region limiting cuts their
//    fragments, but it does not make a small glass ornament free. The blur radius comes mostly
//    from the downsample rather than from kernel width; `fs_blur.frag` argues the tap count.
//    **Those figures are the 1:1 television**; every size here is derived from
//    `surface::viewport()` at first use, not written down. The simulator found out why within an
//    hour of this landing: its drawable is HALF the authored canvas, and a chain hard-coded to
//    1920x1080 grabbed a rect that does not exist, then sampled a UV window that mapped to no part
//    of the screen. On a 1:1 surface both bugs are invisible — which is the whole class of thing
//    the desktop simulator is for. The chain is built once because the drawable never changes
//    after `surface::probe` (no resize on either platform); a surface that could would owe this an
//    invalidation.
// 4. **Every Midgard trap the capture chain documents applies here**, because it is the same
//    machinery: NPOT targets REQUIRE CLAMP_TO_EDGE + LINEAR (core ES2 samples them opaque black
//    under the defaults, with no GL error), `glClear` before each pass spares the tile
//    preserve-load, and RGBA FBO renderability is implementation-defined so completeness is
//    checked and the feature LATCHES OFF rather than crashing. A latched-off blur draws nothing at
//    all and the panel keeps its own opaque ground — the fallback is the old look, not a hole.
//
// Deliberately NOT used by the player's panels (track/chapters/info/overflow). There the frame
// behind the popover is mostly alpha-0 punch-through to the hardware video plane, which GL cannot
// read: blurring it would smear transparency, not video. `docs/` and the player HUD's own notes
// say why that plane is unreachable; nothing here changes it.

/// How many exact halvings the snapshot goes through before the blur taps — **the one knob that
/// decides how much STRUCTURE survives behind the glass, which is a design question, not a perf
/// one.**
///
/// 2 (quarter res) is a properly frosted panel: everything behind it is a colour field. But the
/// lens in `fs_glass.frag` can only bend what is there, and a field of colour looks the same bent —
/// so at 2 the refraction reads as a lit slab rather than as anything being displaced. 1 (half res)
/// keeps coarse shapes recognisable, which is what gives the edge something to visibly distort, at
/// the cost of a less anonymous background and ~1M more fragments through the tap passes.
///
/// This is the tension Apple resolves by blurring LESS than the obvious amount. Both are defensible
/// and the choice belongs to whoever is looking at the television.
const BLUR_REDUCTIONS: usize = 2;
/// Kawase tap offsets, in TEXELS of the final target, one per pass. Widening between passes is
/// what buys a wide penumbra from four taps; both are half-texel-aligned so GL_LINEAR resolves each
/// tap as a 2x2 box. In texels rather than pixels on purpose — the target scales with the drawable,
/// so the blur covers the same FRACTION of the screen on a half-size surface instead of shrinking.
const BLUR_TAPS: [f32; 2] = [1.5, 3.5];
/// The UP pass's tap offset, in texels of the target it writes.
///
/// **This pass is what stops the result looking like enlarged pixels rather than like blur**, and
/// its absence was visible on the television before anything else was. Reducing to a quarter and
/// then letting the panel draw magnify that in ONE bilinear tap is not a blur at all: bilinear
/// magnification is piecewise linear, so the texel lattice survives as faint facets and creases,
/// and the eye reads those as an upscaled image. No number of extra passes DOWN fixes it — the
/// artefact is created by the magnification, after all of them.
///
/// So the chain ends by going back UP one level through the same 4-tap filter (this is the "dual
/// filter" half that was missing). The stored snapshot is then half-res and genuinely smooth, and
/// the per-frame panel draw stays exactly one bilinear tap — the up-filter cost is paid only when
/// the cached snapshot is refreshed, not while that snapshot is reused.
const BLUR_UP_TAP: f32 = 1.25;
const BLUR_PASS_TINT: [f32; 4] = [1.0, 1.0, 1.0, 1.0]; // untinted blit between targets

/// The chain's four target sizes, from the drawable's canvas rect: two exact halvings.
///
/// Exact, not fitted — bilinear minification is a clean 2x2 box only at exactly 2x, and the
/// capture chain's own note records what a single 4x pass costs instead (text shimmer). Odd
/// dimensions floor, which loses at most a pixel column off a blurred backdrop and is not worth a
/// second code path. Pure, so the halving and the floor are host-gradeable.
/// How many passes the chain runs, and therefore which way up the snapshot is stored: every pass
/// flips row order once. The reductions, the taps, and the up pass when there is a level to come
/// back up to.
///
/// **One expression, deliberately.** It was inlined in two places and they disagreed the instant the
/// up pass was added — one said four passes, the other five — which inverted the lens's v axis while
/// leaving the sampling window correct. Nothing warns, and on a symmetric backdrop nothing shows.
///
#[inline]
fn blur_passes() -> usize {
    BLUR_REDUCTIONS + BLUR_TAPS.len() + usize::from(BLUR_REDUCTIONS >= 2)
}
#[inline]
fn blur_is_bottom_up() -> bool {
    blur_passes() % 2 == 0
}

#[inline]
fn blur_dims(vw: c_int, vh: c_int) -> ((c_int, c_int), (c_int, c_int)) {
    let mid = ((vw / 2).max(1), (vh / 2).max(1));
    if BLUR_REDUCTIONS < 2 {
        return (mid, mid); // the taps run at half res; `mid` is then only the first pass's target
    }
    (mid, ((mid.0 / 2).max(1), (mid.1 / 2).max(1)))
}

struct BlurChain {
    grab: c_uint, // the canvas rect of the drawable, copied verbatim
    gw: c_int,
    gh: c_int,
    gx: c_int, // where that rect starts in the drawable (letterbox offset)
    gy: c_int,
    mid: c_uint,
    mid_fbo: c_uint,
    mw: c_int,
    mh: c_int,
    a: c_uint, // quarter-size ping — and where the finished snapshot lands (even pass count)
    a_fbo: c_uint,
    b: c_uint, // pong, same size as `a`
    b_fbo: c_uint,
    sw: c_int,
    sh: c_int,
    /// The texture the finished snapshot lands in, and its size — `mid` after the up pass, or `a`
    /// when there was no level to come back up to. Held rather than re-derived so the draw has one
    /// thing to sample and no copy of the chain's shape.
    out: c_uint,
    /// Is the finished snapshot stored bottom-up (like the `glCopyTexSubImage2D` it started as)?
    ///
    /// Every pass flips row order once, so this is the parity of the pass count — and it is stored
    /// rather than asserted because [`BLUR_REDUCTIONS`] is a knob: the chain ran an even number of
    /// passes at one setting and an odd number at the other, and the first version of this hard-
    /// coded "even" in a test. A wrong answer here draws the page upside down under the panel.
    bottom_up: bool,
    /// The AUTHORED region the live snapshot holds — what [`blur_region_covers`] tests a panel
    /// against, and the space [`blur_uv_rect`] maps screen coordinates out of.
    reg: [f32; 4],
    /// That region in drawable px, a multiple of 4, sitting at the bottom-left of every target.
    ///
    /// **The targets are NOT resized to it**, and that is measured rather than assumed: with the
    /// full-size allocations kept and only the viewports shrunk to a quarter of the area, the chain
    /// went 9.54 → 5.62 ms on the dev set (2026-08-16). Midgard does not resolve tiles nothing drew
    /// into, so a region costs its own area and not the framebuffer's — which is what makes this a
    /// UV-and-viewport change instead of an allocator that has to guess a worst-case panel.
    rw: c_int,
    rh: c_int,
}
/// Snapshots actually taken since the last heartbeat — the REFRESH RATE, measured rather than
/// assumed.
///
/// It exists because "check your cadence actually is what you think" is rule 7 of this
/// investigation's methodology and every other way of checking it needs a profiler armed, which
/// changes the frame rate being measured. A counter costs one relaxed increment on a path that
/// already does a framebuffer copy, and it turns "the blur refreshed every third present" from a
/// claim about the code into a number in the log.
pub(crate) static BLUR_SNAPSHOTS: AtomicU32 = AtomicU32::new(0);

/// Take and clear the snapshot count, for the once/sec heartbeat.
pub(crate) fn take_blur_snapshots() -> u32 {
    BLUR_SNAPSHOTS.swap(0, Ordering::Relaxed)
}

static mut BLURST: Option<BlurChain> = None;
/// Latched off: no FBO, no program, or a failed copy. Callers draw no backdrop and keep their own
/// ground — checked before every snapshot so a failure costs one log line, not a frame loop.
static mut BLUR_OFF: bool = false;
/// A snapshot exists AND still describes what is behind the panel.
static mut BLUR_VALID: bool = false;
static mut BPROG: c_uint = 0;
static mut BL_RECT: c_int = 0;
static mut BL_SCREEN: c_int = 0;
static mut BL_UVRECT: c_int = 0;
static mut BL_TEX: c_int = 0;
static mut BL_TEXEL: c_int = 0;

/// How far in from the panel's edge the lens reaches, in authored px. Wider reads as a thicker
/// slab; past ~40 the "glass" starts to look like a vignette, because the ramp then covers enough
/// of the panel to be seen as shading rather than as an edge.
const GLASS_BEVEL: f32 = 28.0;
/// Peak displacement at the rim, in authored px — how hard the edge bends what is behind it.
///
/// **Large on purpose, and it was three times smaller in the first version.** The source is a
/// quarter-res blur with only coarse structure left in it, so a displacement small enough to
/// "preserve detail" moves nothing an eye can find: the bend has to slide whole light and dark
/// regions to be seen at all. The first attempt compensated with a sharper source at the rim,
/// which reads as the panel getting thinner rather than as refraction — `fs_glass.frag` records
/// why that was removed and this raised instead.
const GLASS_LENS: f32 = 38.0;
/// The lit chamfer's colour. A weight on the overlay ramp rather than a hue, per the theme rule —
/// it is white light, and its ALPHA is the only thing tuned.
const GLASS_EDGE: [f32; 4] = [1.0, 1.0, 1.0, 0.14];
/// Direction TO the light in panel-local space (`+y` is DOWN, so a negative y is above), plus the
/// shading applied to the chamfer facing away from it.
///
/// Up and a little to the left — the direction every drop shadow in this app is already cast from,
/// so the panel is lit by the same imaginary lamp as the cards under it. The counter-shade is what
/// keeps it from reading as an outline: a ring that is bright all the way round is a stroke, and a
/// bevel that is bright on one side and dark on the other is an object.
const GLASS_LIGHT: [f32; 3] = [-0.35, -0.94, 0.45];
/// The rim's SPECULAR: `(axis.x, axis.y, tightness, strength)`.
///
/// The axis is the panel's diagonal, and the shader takes `abs` of the normal's projection onto it,
/// so the reflection catches at **two opposite corners** — top-left and bottom-right — while the
/// other two stay dark. That two-lobe asymmetry is the reading a single lobe cannot give: one lobe
/// is a light source, two are a reflection of one, which is what a glass rim actually shows.
///
/// The tightness sets how far the catch RUNS along the perimeter either side of a corner, and it
/// is the parameter that was wrong first. A rounded rect's normal is constant along each straight
/// side, so the power is the only thing shaping the falloff: at 8 the term is 0.06 on a side
/// against 1.0 on the corner arc, which is two bright dots and nothing else — measured at 265
/// changed pixels on the panel. 3 gives ~0.35 on the sides, i.e. a highlight that travels along the
/// edge and gathers at the corner, which is what a rim reflection looks like.
const GLASS_SPEC: [f32; 4] = [-0.7071, -0.7071, 3.0, 0.80];
/// Dither amplitude, as a fraction of full scale: one 8-bit quantum peak-to-peak (±half). Enough
/// to break a band, below the level at which it reads as grain. Not optional — see `fs_glass.frag`.
const GLASS_NOISE: f32 = 1.0 / 255.0;

static mut GPROG: c_uint = 0;
static mut GL_RECT: c_int = 0;
static mut GL_SCREEN: c_int = 0;
static mut GL_UVRECT: c_int = 0;
static mut GL_TEX: c_int = 0;
static mut GL_TINT: c_int = 0;
static mut GL_RADIUS: c_int = 0;
static mut GL_CH: c_int = 0;
static mut GL_UVPX: c_int = 0;
static mut GL_BEVEL: c_int = 0;
static mut GL_LENS: c_int = 0;
static mut GL_EDGE: c_int = 0;
static mut GL_LIGHT: c_int = 0;
static mut GL_SPEC: c_int = 0;
static mut GL_NOISE: c_int = 0;

/// Drop the cached snapshot: the next [`draw_blur_backdrop`] re-captures.
///
/// `Popover::open` starts every cache lifetime. A cached policy stops there; a dynamic `Glass`
/// policy also calls this on its configured successful-present cadence. Anything changing an
/// underlay outside those policies still owes an explicit invalidation.
pub(crate) fn blur_invalidate() {
    unsafe { BLUR_VALID = false };
}

/// How far outside a panel's own rect the snapshot must actually contain pixels, in authored px.
///
/// Two things reach out past the edge and both are drawn FROM outside it. The lens displaces its
/// sample by up to [`GLASS_LENS`] (38) along the outward normal, so the rim shows what is beside the
/// panel, not under it. And the chain's own kernel spreads: the taps are 1.5 and 3.5 texels at
/// quarter res (4 authored px each) = 20, the up pass 1.25 at half res = 2.5, the reduction's box
/// ~2 — about 24.5 together. 38 + 24.5 is 62.5; this rounds up.
///
/// Short of this the rim samples clamped edge texels, which reads as the glass smearing one colour
/// outward — subtle, and exactly the kind of thing that looks like a shader bug rather than a
/// too-small grab.
const BLUR_REACH: f32 = 68.0;
/// The largest `rise` any popover slides through ([`crate::ui::popover::Popover::painter`]'s second
/// argument). It is 20 everywhere today; the assertion in the tests is what makes raising one a
/// visible decision rather than a silent loss of margin.
const POPOVER_MAX_RISE: f32 = 20.0;
/// What the snapshot actually grabs beyond the panel: [`BLUR_REACH`] plus the slide.
///
/// **The slack keeps the appear slide from forcing extra snapshots.** A popover's first draw is at
/// `appear == 0`, so the region is established while the panel sits a full `rise` away from where it
/// comes to rest; every later frame moves it back INTO that region. Cache hits are therefore
/// containment tests — key on equality, or grab only `BLUR_REACH`, and even a cached popover would
/// recapture on every frame of its appear.
const BLUR_MARGIN: f32 = BLUR_REACH + POPOVER_MAX_RISE;

/// The region a panel at `(x,y,w,h)` needs snapshotted, in authored coords, clamped to the screen.
///
/// Clamping is not a special case: past the edge there is nothing to sample and `CLAMP_TO_EDGE`
/// already gives the only answer available, so a panel against the frame simply gets a shorter
/// margin on that side.
#[inline]
fn blur_region(x: f32, y: f32, w: f32, h: f32) -> [f32; 4] {
    let x0 = (x - BLUR_MARGIN).max(0.0);
    let y0 = (y - BLUR_MARGIN).max(0.0);
    let x1 = (x + w + BLUR_MARGIN).min(SCR_W);
    let y1 = (y + h + BLUR_MARGIN).min(SCR_H);
    [x0, y0, (x1 - x0).max(1.0), (y1 - y0).max(1.0)]
}

/// The smallest region holding both — how a frame with more than one glass surface is served by ONE
/// snapshot instead of two.
///
/// Either side may be the empty marker (a zero-width region), which is what the first frame after a
/// reset carries; the union with it is the other side unchanged rather than a box anchored at the
/// origin.
#[inline]
fn blur_region_union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    if a[2] <= 0.0 || a[3] <= 0.0 {
        return b;
    }
    if b[2] <= 0.0 || b[3] <= 0.0 {
        return a;
    }
    let x0 = a[0].min(b[0]);
    let y0 = a[1].min(b[1]);
    let x1 = (a[0] + a[2]).max(b[0] + b[2]);
    let y1 = (a[1] + a[3]).max(b[1] + b[3]);
    [x0, y0, x1 - x0, y1 - y0]
}

/// What every glass surface drawn in the PREVIOUS frame asked for, unioned — the region this frame's
/// first snapshot is taken at, so a frame holding two of them takes ONE.
///
/// **The previous frame, not this one, and that is the whole trick.** A snapshot has to be taken by
/// the first caller, before anything downstream has had a chance to say what it needs; the only
/// honest predictor of the rest of the frame is what the last one contained. A tab bar plus a row of
/// controls therefore costs two snapshots on the single frame the set CHANGES and one on every frame
/// after — it converges in one frame and, just as importantly, SHRINKS in one frame when a surface
/// goes away, which a monotonically growing union never would.
static mut BLUR_WANT_PREV: [f32; 4] = [0.0; 4];
/// The same union, accumulating over the frame in progress. Rolled into [`BLUR_WANT_PREV`] by
/// [`blur_frame_end`].
static mut BLUR_WANT_CUR: [f32; 4] = [0.0; 4];

/// Close the frame's region accounting. Called once per DRAWN frame, beside `profile::frame_end` —
/// inside the idle gate, because a frame the gate skipped drew no glass and must not be allowed to
/// forget what the last drawn one needed.
pub(crate) fn blur_frame_end() {
    unsafe {
        BLUR_WANT_PREV = *std::ptr::addr_of!(BLUR_WANT_CUR);
        BLUR_WANT_CUR = [0.0; 4];
    }
}

/// Does a cached `reg` still serve a panel at `(x,y,w,h)` — i.e. does it hold every pixel the rim
/// will reach for? Containment, never equality: see [`BLUR_MARGIN`].
#[inline]
fn blur_region_covers(reg: [f32; 4], x: f32, y: f32, w: f32, h: f32) -> bool {
    let need = [
        (x - BLUR_REACH).max(0.0),
        (y - BLUR_REACH).max(0.0),
        (x + w + BLUR_REACH).min(SCR_W),
        (y + h + BLUR_REACH).min(SCR_H),
    ];
    reg[0] <= need[0] + 0.5
        && reg[1] <= need[1] + 0.5
        && reg[0] + reg[2] >= need[2] - 0.5
        && reg[1] + reg[3] >= need[3] - 0.5
}

/// The authored region in DRAWABLE pixels, offset from the canvas rect's top-left, with the size
/// rounded to a multiple of 4.
///
/// Four, because the chain halves twice and a target derived by integer division from an unaligned
/// size does not tile back onto its source — the second reduction would sample half a texel off and
/// the snapshot would creep sideways as a panel moved. Rounding is OUTWARD on the origin and the
/// far edge, then the size is trimmed back to a multiple of 4, which can only ever give away a
/// pixel or three of the margin.
#[inline]
fn blur_region_px(reg: [f32; 4], gw: c_int, gh: c_int) -> (c_int, c_int, c_int, c_int) {
    blur_region_px_align(reg, gw, gh, 4)
}

/// The same rounding, to an arbitrary power-of-two `align`.
///
/// The capture path needs 4 because it halves twice and both halvings must stay registered. The
/// direct path renders the scene straight in at 1/`scale`, so its region has to divide by `scale`
/// exactly or the viewport offset below lands on a fraction of a target pixel and the backdrop
/// creeps sideways as the panel moves.
fn blur_region_px_align(
    reg: [f32; 4],
    gw: c_int,
    gh: c_int,
    align: c_int,
) -> (c_int, c_int, c_int, c_int) {
    let mask = !(align - 1);
    let slack = align - 1;
    let sx = gw as f32 / SCR_W;
    let sy = gh as f32 / SCR_H;
    let x0 = ((reg[0] * sx).floor() as c_int).clamp(0, gw) & mask;
    let y0 = ((reg[1] * sy).floor() as c_int).clamp(0, gh) & mask;
    let x1 = (((reg[0] + reg[2]) * sx).ceil() as c_int + slack).clamp(0, gw);
    let y1 = (((reg[1] + reg[3]) * sy).ceil() as c_int + slack).clamp(0, gh);
    let rw = ((x1 - x0) & mask).clamp(align, (gw - x0).max(align));
    let rh = ((y1 - y0) & mask).clamp(align, (gh - y0).max(align));
    (x0, y0, rw & mask, rh & mask)
}

/// The UV window into the snapshot that a screen-space rect samples.
///
/// **The v axis may run backwards, and which way is not a constant.** Every target in the chain is
/// rendered through `vs_img.vert`, which flips row order once per pass, so the snapshot's storage
/// order is the parity of the pass count — bottom-up (like the `glCopyTexSubImage2D` it started as)
/// after an even number, top-down after an odd one, and [`BLUR_REDUCTIONS`] changes which. The
/// caller passes what the chain actually did. Pure, and tested: an inverted backdrop is the failure
/// this shape hides, and it looks deliberate enough to survive a glance.
///
/// `reg` is the authored region the snapshot holds and `span` the FRACTION of the output texture it
/// occupies — the chain writes into the bottom-left corner of full-size targets rather than into
/// resized ones, so a region is never the whole texture. With the whole screen grabbed and a span of
/// 1 this is exactly the expression it always was, which is what the first test here pins.
#[inline]
fn blur_uv_rect(x: f32, y: f32, w: f32, h: f32, reg: [f32; 4], span: [f32; 2], bottom_up: bool) -> [f32; 4] {
    let fx = (x - reg[0]) / reg[2];
    let fw = w / reg[2];
    let fy = (y - reg[1]) / reg[3];
    let fh = h / reg[3];
    if bottom_up {
        return [fx * span[0], (1.0 - fy) * span[1], fw * span[0], -fh * span[1]];
    }
    [fx * span[0], fy * span[1], fw * span[0], fh * span[1]]
}

/// Build the chain at BOOT, beside [`init_image`].
///
/// Measured, and the reason this is not left lazy: the first menu opening paid **48.1 ms** against
/// **33.4 ms** for every one after it — a shader link plus ~11 MB of texture allocation, landing on
/// a frame the user is looking at. Worse, it lands on the WORST one: a first open also drops the
/// appear animation to 19 presented frames where a later open holds 28. Everything else here is
/// deliberately paid on demand; this part is not, because "on demand" means "while something is
/// animating".
///
/// It is still safe to call nothing at all: [`draw_blur_backdrop`] builds the chain itself if this
/// never ran, which is what keeps the host tests and any future entry point honest.
pub(crate) fn init_blur() {
    if !blur_lazy_init() {
        log("blur: chain unavailable — panels keep their opaque ground");
    }
}

fn blur_lazy_init() -> bool {
    unsafe {
        if BLUR_OFF {
            return false;
        }
        if (*std::ptr::addr_of!(BLURST)).is_some() {
            return true;
        }
        BPROG = match link_program(VS_IMG.as_ptr(), FS_BLUR.as_ptr()) {
            Some(p) => p,
            None => {
                log("blur: prog link failed — backdrop blur off");
                BLUR_OFF = true;
                return false;
            }
        };
        BL_RECT = glGetUniformLocation(BPROG, c"u_trect".as_ptr());
        BL_SCREEN = glGetUniformLocation(BPROG, c"u_tscreen".as_ptr());
        BL_UVRECT = glGetUniformLocation(BPROG, c"u_uvrect".as_ptr());
        BL_TEX = glGetUniformLocation(BPROG, c"u_tex".as_ptr());
        BL_TEXEL = glGetUniformLocation(BPROG, c"u_texel".as_ptr());
        // Per-program constant uniforms, set once (uniforms are per-program state): this program
        // only ever draws one full-target quad, so its rect and UV window never change either.
        use_prog(BPROG);
        glUniform2f(BL_SCREEN, SCR_W, SCR_H);
        glUniform1i(BL_TEX, 0);
        glUniform4f(BL_RECT, 0.0, 0.0, SCR_W, SCR_H);
        glUniform4f(BL_UVRECT, 0.0, 0.0, 1.0, 1.0);

        GPROG = match link_program(VS_IMG.as_ptr(), FS_GLASS.as_ptr()) {
            Some(p) => p,
            None => {
                log("blur: glass prog link failed — backdrop blur off");
                BLUR_OFF = true;
                return false;
            }
        };
        GL_RECT = glGetUniformLocation(GPROG, c"u_trect".as_ptr());
        GL_SCREEN = glGetUniformLocation(GPROG, c"u_tscreen".as_ptr());
        GL_UVRECT = glGetUniformLocation(GPROG, c"u_uvrect".as_ptr());
        GL_TEX = glGetUniformLocation(GPROG, c"u_tex".as_ptr());
        GL_TINT = glGetUniformLocation(GPROG, c"u_tint".as_ptr());
        GL_RADIUS = glGetUniformLocation(GPROG, c"u_iradius".as_ptr());
        GL_CH = glGetUniformLocation(GPROG, c"u_ch".as_ptr());
        GL_UVPX = glGetUniformLocation(GPROG, c"u_uvpx".as_ptr());
        GL_BEVEL = glGetUniformLocation(GPROG, c"u_bevel".as_ptr());
        GL_LENS = glGetUniformLocation(GPROG, c"u_lens".as_ptr());
        GL_EDGE = glGetUniformLocation(GPROG, c"u_edge".as_ptr());
        GL_LIGHT = glGetUniformLocation(GPROG, c"u_light".as_ptr());
        GL_SPEC = glGetUniformLocation(GPROG, c"u_spec".as_ptr());
        GL_NOISE = glGetUniformLocation(GPROG, c"u_noise".as_ptr());
        use_prog(GPROG);
        glUniform2f(GL_SCREEN, SCR_W, SCR_H);
        glUniform1i(GL_TEX, 0);
        // Material constants and the orientation term: all fixed for the life of the process, and
        // uniforms are per-program state, so none of them belongs in the draw.
        glUniform1f(GL_BEVEL, GLASS_BEVEL);
        glUniform1f(GL_LENS, GLASS_LENS);
        glUniform4fv(GL_EDGE, 1, GLASS_EDGE.as_ptr());
        glUniform3f(GL_LIGHT, GLASS_LIGHT[0], GLASS_LIGHT[1], GLASS_LIGHT[2]);
        glUniform4fv(GL_SPEC, 1, GLASS_SPEC.as_ptr());
        glUniform1f(GL_NOISE, GLASS_NOISE);
        // `GL_UVPX` is NOT set here. It was, back when the grab was always the whole screen and one
        // authored pixel was therefore always `1/SCR_W` of the texture — but the snapshot is a
        // REGION now and that ratio moves with it, so it belongs to the draw. `draw_blur_backdrop`
        // says what goes wrong if it is left behind.
        use_prog(PROG);

        let (gx, gy, gw, gh) = crate::surface::viewport();
        let ((mw, mh), (sw, sh)) = blur_dims(gw, gh);
        let build = || -> Option<BlurChain> {
            let grab = cap_tex(gw, gh);
            let (mid, mid_fbo) = fbo_target(mw, mh, "blur")?;
            let (a, a_fbo) = fbo_target(sw, sh, "blur")?;
            let (b, b_fbo) = fbo_target(sw, sh, "blur")?;
            // The up pass exists only when there is a level to come back up to; with one reduction
            // `mid` IS the tap target, so the chain ends where it already is.
            let ups = BLUR_REDUCTIONS >= 2;
            Some(BlurChain { grab, gw, gh, gx, gy, mid, mid_fbo, mw, mh, a, a_fbo, b, b_fbo, sw, sh,
                out: if ups { mid } else { a },
                bottom_up: blur_is_bottom_up(),
                // No live snapshot yet; `BLUR_VALID` is what gates reading these, and the first
                // `blur_snapshot` sets both to a real region before anything samples them.
                reg: [0.0, 0.0, SCR_W, SCR_H], rw: gw, rh: gh })
        };
        match build() {
            Some(c) => {
                BLURST = Some(c);
                true
            }
            None => {
                BLUR_OFF = true; // fbo_target already logged the status
                false
            }
        }
    }
}

/// Grab the frame as drawn so far and reduce it to the small blurred snapshot.
///
/// Main-thread only, and it must run with the default framebuffer bound. Restores framebuffer 0,
/// the viewport and blend exactly, for the same reason the capture chain does: whatever runs after
/// it assumes all three.
///
/// # Device measurements (2026-08-16)
///
/// The original full-screen chain measured 9.54 ms. Limiting every pass to the requested panel
/// region reduced a 630×790 Sort snapshot to **3.43 ms** (`copy=.71`, reductions `1.23`, two taps
/// `.74` (now reported separately as `blur.tap1`/`blur.tap2`), up `.75`). The approved dynamic Account scene measured about **3.9 ms per refresh**.
/// These are legacy `profile::phase` wall times with `glFinish` around every phase. They are not
/// normal pipelined frame time or hardware GPU timestamps, so they are historical sizing clues,
/// not the baseline for the direct-render experiment.
///
/// Two full-size reduction points once suggested a `0.49 ms/pass + 2.7 ns/fragment` sizing model.
/// The final region measurement (`blur.taps=.74` for two passes) does not satisfy a global
/// `0.49 ms/pass` floor, so that fit is deliberately not used as an invariant or as proof that no
/// further pass fusion can help. Use measured regions and end-to-end A/Bs; `docs/liquid-glass.md`
/// records the current envelope.
fn blur_snapshot(reg: [f32; 4]) {
    unsafe {
        if !blur_lazy_init() {
            return;
        }
        let Some(c) = (*std::ptr::addr_of!(BLURST)).as_ref() else { return };
        BLUR_SNAPSHOTS.fetch_add(1, Ordering::Relaxed);

        // Drain first: `glGetError` reports the OLDEST error since it was last called, so checking
        // it after the copy without clearing it attributes somebody else's (possibly harmless, and
        // possibly from another module entirely) to this one — which is exactly how the first
        // version of this turned itself off on a healthy driver.
        while glGetError() != GL_NO_ERROR {}
        // Split into exact phases for the asynchronous timer-query and serialized HWCNT diagnostic
        // modes. The candidates are a framebuffer copy/resolve and five render-target passes.
        // The phase wrapper is an inline passthrough in a release build.
        use crate::ui::profile::phase;
        // The region, in drawable px and 4-aligned, landing at the BOTTOM-LEFT of every target. Each
        // pass then draws a full quad into a viewport of the region's own size and samples the
        // matching corner of its source, so the whole chain is the same shape it always was, one
        // scale factor down. Targets keep their full allocation — measured, see `BlurChain::rw`.
        let (rx, ry, rw, rh) = blur_region_px(reg, c.gw, c.gh);
        // Source windows, per pass: the fraction of each texture the live region occupies.
        let win = |uw: c_int, uh: c_int, tw: c_int, th: c_int| {
            (uw as f32 / tw as f32, uh as f32 / th as f32)
        };
        let e = phase("blur.copy", || {
            glBindTexture(GL_TEXTURE_2D, c.grab);
            // `glCopyTexSubImage2D` reads the framebuffer bottom-left-origin while the region is
            // measured from the canvas TOP, so the row has to be flipped into window space here.
            // Getting this wrong grabs a strip from the other end of the screen — which under a
            // blur looks like a plausible backdrop, just not the one behind the panel.
            glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, c.gx + rx, c.gy + c.gh - (ry + rh), rw, rh);
            glGetError()
        });
        if e != GL_NO_ERROR {
            // The same failure `capture` guards: a window config without alpha bits makes the copy
            // a silent no-op, which would read as "the blur is black" rather than as a fault.
            log(&format!("blur: CopyTexSubImage error=0x{e:x} — backdrop blur off"));
            BLUR_OFF = true;
            return;
        }

        glDisable(GL_BLEND);
        // The exact-2x reductions (bilinear == a 2x2 box at exactly 2x, which is where most of the
        // radius comes from), then the Kawase passes ping-ponging between the two small targets.
        // The LAST reduction always lands in `a`, whatever `BLUR_REDUCTIONS` is, because that is
        // where the tap loop below expects its input.
        //
        // A one-pass 4-tap 4x reduction was built against the old FULL-SCREEN chain and measured
        // slower: 2.89 ms against the pair's 2.74. The idea is sound on paper and the filter
        // is exactly identical (a target texel covers a 4x4 source block; four bilinear fetches
        // placed on the four 2x2 sub-blocks' shared corners each return that sub-block's mean, so
        // the four averaged are all sixteen texels at equal weight — which is what a 2x box
        // followed by a 2x box produces).
        //
        // What the model misses is LOCALITY. The merged pass gathers a 4x4 footprint per output
        // fragment out of a 1920-wide texture, and adjacent output fragments are four texels apart,
        // so nothing is reused between them and the texture cache misses on essentially every
        // fetch. The two-step version reads ADJACENT texels in both passes and streams. The gather
        // costs more than the pass it removes — measured on the dev set 2026-08-16, and the whole
        // reason `blur.reduce` is split into two phases is so a re-measurement is one log line away.
        // The final region-limited viewport was never A/B'd against this alternative, so that old
        // full-screen result is evidence for today's choice, not a universal rejection.
        //
        // Each entry is (target fbo, source tex, region size in the TARGET, source window). The
        // source window is the only thing the region added: `draw_tex_core` takes an explicit uv
        // rect, where the plain `draw_tex` these used to call hard-codes the whole texture — which
        // with a region grabbed into a corner would stretch the entire (mostly stale) target across
        // the pass and blur the region together with whatever the last panel left behind it.
        let (r2w, r2h) = ((rw / 2).max(1), (rh / 2).max(1));
        let (r4w, r4h) = ((rw / 4).max(1), (rh / 4).max(1));
        let reductions: &[(c_uint, c_uint, c_int, c_int, (f32, f32))] = if BLUR_REDUCTIONS >= 2 {
            &[
                (c.mid_fbo, c.grab, r2w, r2h, win(rw, rh, c.gw, c.gh)),
                (c.a_fbo, c.mid, r4w, r4h, win(r2w, r2h, c.mw, c.mh)),
            ]
        } else {
            &[(c.a_fbo, c.grab, r2w, r2h, win(rw, rh, c.gw, c.gh))]
        };
        // ONE PHASE PER PASS, deliberately: this split is what MEASURED the cost model above, and
        // it is left in place because it is the only way to re-measure it. `phase` takes a
        // `&'static str`, so the names are written out rather than indexed.
        for (i, &(fbo, src, w, h, uv)) in reductions.iter().enumerate() {
            let name = if i == 0 { "blur.reduce1" } else { "blur.reduce2" };
            phase(name, || {
                glBindFramebuffer(GL_FRAMEBUFFER, fbo);
                glViewport(0, 0, w, h);
                glClear(GL_COLOR_BUFFER_BIT);
                // NO_RIM, never a null pointer: `draw_tex_core` hands both colours straight to
                // `glUniform4fv`, which dereferences them unconditionally — a null segfaults inside
                // the driver with no Rust panic and no log line (caught by the simulator, 2026-08-16).
                draw_tex_core(src, 0.0, 0.0, SCR_W, SCR_H, [0.0, 0.0, uv.0, uv.1], 0.0,
                    BLUR_PASS_TINT.as_ptr(), 0.0, NO_RIM.as_ptr(), 0.0, 0.0, 0.0, NO_RIM.as_ptr());
            });
        }
        // The taps run at whichever level the reductions ended on: quarter with two, half with one.
        let (tw, th) = if BLUR_REDUCTIONS >= 2 { (r4w, r4h) } else { (r2w, r2h) };
        let tap_uv = win(tw, th, c.sw, c.sh);
        for (i, taps) in BLUR_TAPS.iter().enumerate() {
            let name = if i == 0 { "blur.tap1" } else { "blur.tap2" };
            phase(name, || {
                glViewport(0, 0, tw, th);
                // even pass -> a into b, odd -> b into a; with an even tap count the finished
                // snapshot lands back in `a`, which is what `draw_blur_backdrop` samples.
                let (fbo, src) = if i % 2 == 0 { (c.b_fbo, c.a) } else { (c.a_fbo, c.b) };
                glBindFramebuffer(GL_FRAMEBUFFER, fbo);
                glClear(GL_COLOR_BUFFER_BIT);
                use_prog(BPROG); // already bound unless the reduction path ran `draw_tex`
                glUniform4f(BL_UVRECT, 0.0, 0.0, tap_uv.0, tap_uv.1);
                // Offsets stay in TEXELS of the texture, which the region does not change — the
                // texture is the same size, only less of it is live.
                glUniform2f(BL_TEXEL, taps / c.sw as f32, taps / c.sh as f32);
                glBindTexture(GL_TEXTURE_2D, src);
                glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
            });
        }

        // Back UP one level through the same filter — the half of a dual-filter blur that stops the
        // result reading as enlarged pixels. See `BLUR_UP_TAP`. `mid` is free by now: it was the
        // first reduction's target and nothing has read it since the second reduction.
        phase("blur.up", || {
            if c.out != c.a {
                glBindFramebuffer(GL_FRAMEBUFFER, c.mid_fbo);
                glViewport(0, 0, r2w, r2h);
                glClear(GL_COLOR_BUFFER_BIT);
                use_prog(BPROG);
                glUniform4f(BL_UVRECT, 0.0, 0.0, tap_uv.0, tap_uv.1);
                glUniform2f(BL_TEXEL, BLUR_UP_TAP / c.mw as f32, BLUR_UP_TAP / c.mh as f32);
                glBindTexture(GL_TEXTURE_2D, c.a);
                glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
            }
        });

        glBindFramebuffer(GL_FRAMEBUFFER, 0);
        let (vx, vy, vw, vh) = crate::surface::viewport();
        glViewport(vx, vy, vw, vh);
        glEnable(GL_BLEND);
        // Publish what was actually grabbed, not what was asked for: `blur_region_px` aligns and
        // clamps, so the region the draw maps out of is the aligned one or the panel's sampling
        // window is off by up to three drawable pixels — a slow sideways creep as a panel moves.
        let sx = c.gw as f32 / SCR_W;
        let sy = c.gh as f32 / SCR_H;
        let live = [rx as f32 / sx, ry as f32 / sy, rw as f32 / sx, rh as f32 / sy];
        crate::ui::profile::note_blur_config(
            live, rx, ry, rw, rh, c.gw, c.gh, c.mw, c.mh, c.sw, c.sh,
        );
        if let Some(c) = (*std::ptr::addr_of_mut!(BLURST)).as_mut() {
            c.reg = live;
            c.rw = rw;
            c.rh = rh;
        }
        BLUR_VALID = true;
    }
}

/// Scale divisor for the DIRECT backdrop path; 0 means the capture path (the shipped default).
///
/// Set once at boot from `/tmp/plxnative-blurdirect`. Only powers of two from 4 up are accepted,
/// because the taps ping-pong between `a` and `b`, which are allocated at a quarter of the canvas:
/// a scale of 2 would need a second half-size target and 2 MB more, which is a separate question
/// from whether the architecture is worth having at all.
static mut BLUR_DIRECT: u32 = 0;

/// True only while [`blur_snapshot_direct`] is running the page draw as a blur SOURCE.
///
/// Three things in the tree must behave differently during that pass, and every one of them is a
/// correctness issue rather than a nicety:
///
/// * `draw_blur_backdrop` must refuse outright. `home_draw` contains a glass owner of its own (the
///   tab track, under `plxnative-glasstabs`), so without this the source pass re-enters
///   `blur_snapshot`, which copies framebuffer 0 — reading the panel while an FBO is bound — and
///   then rebinds framebuffer 0 and the full-resolution viewport in the middle of the pass.
/// * `ui::profile::phase` must not select. The page's own phases (`hm.grid`, `main.ui`, …) would
///   otherwise record twice per frame under one name, mixing a quarter-resolution sample into the
///   full-resolution mean that the whole experiment is priced by.
/// * the per-frame card counters must not accumulate, or the framedrop tool reports twice the
///   composites the panel actually shows.
static mut BLUR_IN_PASS: bool = false;

/// Is the page currently being drawn as a low-resolution blur source rather than for the panel?
#[inline]
pub(crate) fn blur_source_pass() -> bool {
    unsafe { BLUR_IN_PASS }
}

/// The authored rect a blur source pass may draw into; nothing outside it can affect the result.
static mut CULL_RECT: Option<[f32; 4]> = None;

/// Should this quad be skipped entirely?
///
/// The scissor bounds what a source pass may WRITE, but on a tile-based GPU it does not stop the
/// tiler binning geometry or the fragment jobs walking tiles: measured on the television, a source
/// pass whose live area is 152x99 processed **1193 tiles where 70 would do**, because the viewport
/// that maps authored space into the target necessarily spans the whole canvas. Refusing the draw
/// call is the only thing that removes the work, and it is exact rather than conservative — the
/// backdrop is a crop of the page, so a quad that misses the region cannot contribute a fragment
/// to it.
///
/// Always `false` outside a source pass, so the visible frame is drawn exactly as it always was.
#[inline]
pub(crate) fn culled(x: f32, y: f32, w: f32, h: f32) -> bool {
    match unsafe { CULL_RECT } {
        None => false,
        // A slack margin covers the AA bleed and the SDF rim, which paint a pixel or so beyond the
        // rect every primitive declares.
        Some(r) => {
            const SLACK: f32 = 4.0;
            x + w < r[0] - SLACK
                || y + h < r[1] - SLACK
                || x > r[0] + r[2] + SLACK
                || y > r[1] + r[3] + SLACK
        }
    }
}

/// Restores framebuffer 0 and everything the source pass changed, ON EVERY EXIT PATH.
///
/// This is a `Drop` guard and not straight-line code for one specific reason: `home_draw` opens
/// with `ui::guard`, which CATCHES a panic and returns normally. A panic anywhere in the page
/// would otherwise leave the FBO bound with the region viewport set — and nothing else in the app
/// ever binds framebuffer 0, so every subsequent frame would render into a 480x270 texture and the
/// television would show a frozen picture with no crash, no log line and no way back.
struct DirectPass;

impl Drop for DirectPass {
    fn drop(&mut self) {
        unsafe {
            BLUR_IN_PASS = false;
            CLIP_TARGET = None;
            CULL_RECT = None;
            glDisable(GL_SCISSOR_TEST);
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
            let (vx, vy, vw, vh) = crate::surface::viewport();
            glViewport(vx, vy, vw, vh);
            glEnable(GL_BLEND);
        }
    }
}

/// Is the direct path armed, and at what axis divisor? `None` selects the capture path.
#[inline]
pub(crate) fn blur_direct_scale() -> Option<u32> {
    let scale = unsafe { BLUR_DIRECT };
    (scale >= 4).then_some(scale)
}

/// Arm the direct backdrop path at 1/`scale` per axis. `scale < 4` disarms it.
pub(crate) fn set_blur_direct(scale: u32) {
    unsafe { BLUR_DIRECT = if scale.is_power_of_two() { scale } else { 0 } };
}

/// The region a direct source pass should be taken at THIS frame, or `None` to do nothing.
///
/// `Some` requires three things at once: the direct path armed, a refresh actually due, and a
/// region to take it at. The region is the PREVIOUS drawn frame's complete union — the same
/// `BLUR_WANT_PREV` the capture path unions into, and the only thing known this early, because the
/// current frame's needs are recorded by the glass surfaces themselves and they have not drawn
/// yet. On the first frame a panel appears that union is empty and this answers `None`; the
/// capture path then takes that one frame the way it always has, and the direct path picks it up
/// from the next present onward. One frame of the old behaviour at activation is the price of
/// hooking before the page draws, which is the only place a second scene pass can go.
pub(crate) fn blur_direct_region() -> Option<[f32; 4]> {
    unsafe {
        blur_direct_scale()?;
        if BLUR_VALID {
            return None;
        }
        let prev = *std::ptr::addr_of!(BLUR_WANT_PREV);
        (prev[2] > 0.0 && prev[3] > 0.0).then_some(prev)
    }
}

/// The backdrop source, rendered by DRAWING THE SCENE AGAIN at 1/`scale` per axis, instead of
/// copying framebuffer 0 and reducing it twice.
///
/// This is the experiment `docs/backdrop-blur-profiling.md` sizes. The capture path spends a
/// `glCopyTexSubImage2D` of the region plus two 2x reduction passes to arrive at a quarter-
/// resolution image of what is behind the panel; the renderer is immediate-mode and holds no
/// display list, but it is also a pure function of UI state, so the same image can be produced by
/// running the page draw a second time into a small target. Three passes replace six.
///
/// # How the scene lands in a cropped, scaled target with no shader change
///
/// Both vertex shaders map an authored pixel with `ndc = px / u_screen * 2 - 1` and emit
/// `-ndc.y`. Nothing in that depends on the target's size, so a SCALED, NEGATIVE-ORIGIN viewport
/// is enough to place any sub-rectangle of the authored canvas anywhere in any target:
///
/// ```text
/// glViewport(-rx/scale, -(gh - ry - rh)/scale, gw/scale, gh/scale)
/// ```
///
/// Solving `vx + (rx/gw)*vw == 0` and `vx + ((rx+rw)/gw)*vw == rw/scale` gives `vw = gw/scale` and
/// `vx = -rx/scale`; the y row falls out the same way once the shaders' flip is accounted for, and
/// lands on the canvas-bottom distance because GL's viewport origin is bottom-left while the
/// region is measured from the canvas top. A negative viewport origin is ordinary — the viewport
/// is an affine map, not a clip — and the scissor below is what bounds the writes.
///
/// # Orientation, which is the easiest thing here to get wrong
///
/// The scene shaders' flip puts authored y=0 at the target's TOP row, so the direct render is
/// stored bottom-up — the same orientation `glCopyTexSubImage2D` produces, since a window copy is
/// bottom-up too. The chain that follows is three passes where the capture path runs five, and
/// both are ODD, so the finished snapshot is top-down either way and `bottom_up` stays `false`.
/// Neither [`blur_uv_rect`] nor the `u_uvpx` V sign in `fs_glass` changes. That is luck, not
/// design; if a pass is ever added or removed here, this is the line that has to move with it.
///
/// Returns `false` if it could not run, in which case the caller must fall back to the capture
/// path — the snapshot is left untouched and no GL state has changed.
pub(crate) fn blur_snapshot_direct(reg: [f32; 4], draw_scene: &mut dyn FnMut()) -> bool {
    unsafe {
        let Some(scale) = blur_direct_scale() else { return false };
        if !blur_lazy_init() {
            return false;
        }
        let Some(c) = (*std::ptr::addr_of!(BLURST)).as_ref() else { return false };
        BLUR_SNAPSHOTS.fetch_add(1, Ordering::Relaxed);
        let step = scale as c_int;
        let (rx, ry, rw, rh) = blur_region_px_align(reg, c.gw, c.gh, step);
        let (tw, th) = ((rw / step).max(1), (rh / step).max(1));
        // The taps ping-pong through `a`/`b`, so the scene has to fit them. At scale 4 the live
        // area is exactly their allocation; anything larger is a caller error, not a clamp.
        if tw > c.sw || th > c.sh {
            log(&format!("blur direct: {tw}x{th} exceeds the {}x{} tap targets", c.sw, c.sh));
            return false;
        }
        // `glViewport` takes integers and the divisions below carry the scale, so a canvas that
        // does not divide by `scale` would place the region a fraction of a target pixel out and
        // the backdrop would creep as the panel moved. The region itself is already aligned.
        if c.gw % step != 0 || c.gh % step != 0 {
            log(&format!("blur direct: canvas {}x{} does not divide by {scale}", c.gw, c.gh));
            return false;
        }
        while glGetError() != GL_NO_ERROR {}

        let vx = -rx / step;
        let vy = -(c.gh - ry - rh) / step;
        use crate::ui::profile::phase;
        phase("blur.scene", || {
            // Armed before anything else, and disarmed by `DirectPass::drop` however this exits.
            BLUR_IN_PASS = true;
            let _restore = DirectPass;
            glBindFramebuffer(GL_FRAMEBUFFER, c.a_fbo);
            glViewport(vx, vy, c.gw / step, c.gh / step);
            // The viewport places the image; the scissor is what stops the rest of the page
            // writing over the tap targets' other content — and it bounds `glClear`, which is
            // viewport-independent. `home_draw` opens with its own `frame_clear`, so the ground
            // is laid inside this box rather than across the whole allocation.
            glEnable(GL_SCISSOR_TEST);
            glScissor(0, 0, tw, th);
            // The same triple the viewport just took, so `Painter::clip` lands on the same pixels.
            CLIP_TARGET = Some((vx, vy, c.gw as f32 / SCR_W / step as f32, tw, th));
            // Authored-space bounds for the draw-call cull. `reg` is already what the region was
            // aligned to, so this and the scissor describe the same rectangle.
            CULL_RECT = Some([
                rx as f32 * SCR_W / c.gw as f32,
                ry as f32 * SCR_H / c.gh as f32,
                rw as f32 * SCR_W / c.gw as f32,
                rh as f32 * SCR_H / c.gh as f32,
            ]);
            // The scene expects the ordinary blend state; `glBlendFuncSeparate` is set once at
            // init and never changed, so enabling is the whole requirement.
            glEnable(GL_BLEND);
            draw_scene();
        });

        glDisable(GL_BLEND);
        let win = |uw: c_int, uh: c_int, tex_w: c_int, tex_h: c_int| {
            (uw as f32 / tex_w as f32, uh as f32 / tex_h as f32)
        };
        let tap_uv = win(tw, th, c.sw, c.sh);
        // Hold the AUTHORED radius fixed as the source resolution moves: a tap offset is in source
        // texels, and a texel covers `scale` authored pixels, so the offsets that give the shipped
        // look at quarter resolution have to shrink in proportion at any finer divisor. Without
        // this a scale sweep changes two variables at once and measures neither.
        let tap_k = 4.0 / scale as f32;
        for (i, taps) in BLUR_TAPS.iter().enumerate() {
            let name = if i == 0 { "blur.tap1" } else { "blur.tap2" };
            phase(name, || {
                glViewport(0, 0, tw, th);
                let (fbo, src) = if i % 2 == 0 { (c.b_fbo, c.a) } else { (c.a_fbo, c.b) };
                glBindFramebuffer(GL_FRAMEBUFFER, fbo);
                glClear(GL_COLOR_BUFFER_BIT);
                use_prog(BPROG);
                glUniform4f(BL_UVRECT, 0.0, 0.0, tap_uv.0, tap_uv.1);
                let off = taps * tap_k;
                glUniform2f(BL_TEXEL, off / c.sw as f32, off / c.sh as f32);
                glBindTexture(GL_TEXTURE_2D, src);
                glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
            });
        }

        // Up to half resolution, exactly as the capture path does, so `out` is `mid` and the glass
        // draw is bit-for-bit the same consumer in both modes. At a divisor beyond 4 this is a
        // larger magnification than 2x, which is precisely the quality question the scale sweep is
        // for; the filter itself is unchanged.
        let (r2w, r2h) = ((rw / 2).max(1), (rh / 2).max(1));
        phase("blur.up", || {
            glBindFramebuffer(GL_FRAMEBUFFER, c.mid_fbo);
            glViewport(0, 0, r2w, r2h);
            glClear(GL_COLOR_BUFFER_BIT);
            use_prog(BPROG);
            glUniform4f(BL_UVRECT, 0.0, 0.0, tap_uv.0, tap_uv.1);
            glUniform2f(BL_TEXEL, BLUR_UP_TAP / c.mw as f32, BLUR_UP_TAP / c.mh as f32);
            glBindTexture(GL_TEXTURE_2D, c.a);
            glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        });

        glBindFramebuffer(GL_FRAMEBUFFER, 0);
        let (vx, vy, vw, vh) = crate::surface::viewport();
        glViewport(vx, vy, vw, vh);
        glEnable(GL_BLEND);
        let e = glGetError();
        if e != GL_NO_ERROR {
            log(&format!("blur direct: GL error=0x{e:x} — falling back to the capture path"));
            BLUR_DIRECT = 0;
            return false;
        }

        let sx = c.gw as f32 / SCR_W;
        let sy = c.gh as f32 / SCR_H;
        let live = [rx as f32 / sx, ry as f32 / sy, rw as f32 / sx, rh as f32 / sy];
        crate::ui::profile::note_blur_config(
            live, rx, ry, rw, rh, c.gw, c.gh, c.mw, c.mh, tw, th,
        );
        if let Some(c) = (*std::ptr::addr_of_mut!(BLURST)).as_mut() {
            c.reg = live;
            c.rw = rw;
            c.rh = rh;
            c.out = c.mid;
            // Three passes, odd like the capture path's five: the snapshot is top-down.
            c.bottom_up = false;
        }
        BLUR_VALID = true;
        true
    }
}

/// Draw the frosted backdrop for a panel at `(x,y,w,h)` with corner `radius`, capturing the
/// snapshot first if there isn't a live one. Returns whether anything was drawn — `false` means the
/// feature is latched off and the caller's own ground is the whole panel.
///
/// `rest` is where the panel comes to REST, and it is the rect the region is built around — not
/// `(x,y,w,h)`, which is where this frame draws it. A popover slides into place over its appear
/// spring, so the two differ for the whole of that animation; keying the region on the moving rect
/// would fail containment on nearly every frame and add policy-independent captures throughout the
/// slide. Callers with no motion (the tab bar) pass their own rect twice.
pub(crate) fn draw_blur_backdrop(x: f32, y: f32, w: f32, h: f32, rest: [f32; 4], radius: f32, tint: *const f32) -> bool {
    unsafe {
        // A glass surface met while drawing the page AS a blur source draws nothing at all. It
        // cannot draw itself — the snapshot it would sample is the target currently bound — and it
        // must not RECORD a need or take a capture either, both of which would run inside the FBO.
        // `false` is also the right picture: the caller falls back to its opaque ground, which is
        // what belongs under a blur anyway. See `BLUR_IN_PASS`.
        if BLUR_IN_PASS {
            return false;
        }
        // Declare what this surface needs BEFORE deciding whether to snapshot, so a frame's second
        // glass element is on record even if the first one is what ends up taking the capture.
        let need = blur_region(rest[0], rest[1], rest[2], rest[3]);
        BLUR_WANT_CUR = blur_region_union(*std::ptr::addr_of!(BLUR_WANT_CUR), need);
        // Containment, not equality: a region grabbed around the panel at rest already holds
        // everything the panel needs at every point of its slide. `blur_invalidate` is what forces
        // a retake when the PAGE changes; this only retakes when the cached region cannot serve.
        let stale = (*std::ptr::addr_of!(BLURST))
            .as_ref()
            .is_none_or(|c| !blur_region_covers(c.reg, x, y, w, h));
        if !BLUR_VALID || stale {
            // Grab what the LAST frame turned out to need, unioned with what this caller needs —
            // never `need` alone. A miss that replaces the region instead of growing it is what
            // makes two neighbouring glass controls ping-pong: each retakes the other's region
            // every frame, two full chains, worse than not limiting the grab at all. A second
            // element inside one grab adds only its composite fragments; a pair at opposite
            // corners instead expands the shared snapshot toward the whole frame.
            let want = blur_region_union(*std::ptr::addr_of!(BLUR_WANT_PREV), need);
            blur_snapshot(want);
        }
        if BLUR_OFF || !BLUR_VALID {
            return false;
        }
        let Some(c) = (*std::ptr::addr_of!(BLURST)).as_ref() else { return false };
        // How much of `out` the region occupies. `out` is `mid` after the up pass (half res) or `a`
        // without one (whatever the reductions ended at), so the live size follows the same rule.
        let span = if c.out == c.mid {
            [(c.rw / 2) as f32 / c.mw as f32, (c.rh / 2) as f32 / c.mh as f32]
        } else {
            let d = 1 << BLUR_REDUCTIONS;
            [(c.rw / d) as f32 / c.sw as f32, (c.rh / d) as f32 / c.sh as f32]
        };
        let uv = blur_uv_rect(x, y, w, h, c.reg, span, c.bottom_up);
        use_prog(GPROG);
        glBindTexture(GL_TEXTURE_2D, c.out);
        // **Per DRAW, not per init.** This is the UV travelled by one authored screen pixel, and it
        // is what turns the lens's displacement (in px) into a texture offset. It was a boot-time
        // constant of `1/SCR_W` while the grab was always the whole screen; against a region it is
        // `span / region size`, which for a 770px-wide region is 2.5x larger. Left at the old value
        // the 38px lens would have silently become a ~15px one — the rim would still refract, just
        // less, with nothing anywhere to say so.
        glUniform2f(
            GL_UVPX,
            span[0] / c.reg[2],
            if c.bottom_up { -span[1] / c.reg[3] } else { span[1] / c.reg[3] },
        );
        glUniform4fv(GL_TINT, 1, tint);
        glUniform4f(GL_UVRECT, uv[0], uv[1], uv[2], uv[3]);
        glUniform1f(GL_RADIUS, radius);
        glUniform2f(GL_CH, w * 0.5, h * 0.5);
        glUniform4f(GL_RECT, x, y, w, h);
        crate::ui::profile::phase("glass.composite", || {
            glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        });
        true
    }
}

// ============================== UI self-capture ==============================
// GL side of the dev capture stream (crate::capture): grab our own back buffer,
// GPU-downscale it, and read the small result back — the fast path the external
// ~200ms/frame capture service can't offer. UI plane ONLY: glReadPixels cannot
// see the hardware video overlay plane (that's composited by the TV, outside our
// context). Design per the verified plan (2026-07-22 workflow):
//
// - TWO chains (parity ping-pong): the chain written this capture is never the
//   chain in flight from the last one — kills Mali's CopyTex-into-referenced-
//   texture ghosting AND lets us read a chain rendered >=1 capture-cycle (>=33ms)
//   ago, so the read degenerates to a driver memcpy instead of the 155ms
//   window-surface pipeline drain we measured.
// - Downscale is exact-2x per pass (2x bilinear == 2x2 box): 1920x1080 -> mid
//   960x540 -> out 480x270. A single 4x pass would sample only 2x2 of each 4x4
//   block -> text shimmer. Both output sizes are always allocated; the requested
//   size only selects which FBO is the pass target (no realloc on switch, no
//   size races — dims are latched per chain at write time).
// - All six textures are NPOT: CLAMP_TO_EDGE + LINEAR are REQUIRED at creation
//   (core ES2 samples an NPOT texture as opaque black under the REPEAT +
//   NEAREST_MIPMAP_LINEAR defaults — no GL error, just black), and mipmaps are
//   illegal. GL_RGBA unsized everywhere (GL_RGBA8 is not a valid ES2 token).
// - RGBA-texture FBO renderability is implementation-defined in core ES2, so
//   completeness is checked; on failure capture latches off (logged) rather than
//   crashing. Midgard exposes OES_rgb8_rgba8 in practice.
// - Y orientation: every full-quad IPROG pass flips row order once (vs_img does
//   gl_Position.y = -ndc.y with v_cuv = a_pos), glReadPixels is bottom-up memory
//   order (NOT a flip). CopyTexSubImage leaves cap_tex bottom-up; 1 pass (960 out)
//   -> top-down (correct); 2 passes (480 out) -> bottom-up again. The per-frame
//   `flip` flag tells the encoder to walk rows in reverse. No CPU flip cost — the
//   RGBA->RGB strip touches every row anyway.
// Main-thread only (like all gfx state).

const GL_FRAMEBUFFER: c_uint = 0x8D40;
const GL_COLOR_ATTACHMENT0: c_uint = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: c_uint = 0x8CD5;
const GL_PACK_ALIGNMENT: c_uint = 0x0D05;
const GL_NO_ERROR: c_uint = 0;

const CAP_W: c_int = 1920;
const CAP_H: c_int = 1080;
const CAP_MID_W: c_int = 960;
const CAP_MID_H: c_int = 540;
const CAP_OUT_W: c_int = 480;
const CAP_OUT_H: c_int = 270;
const CAP_TINT: [f32; 4] = [1.0, 1.0, 1.0, 1.0]; // untinted blit

struct CapChain {
    grab: c_uint,     // 1920x1080 back-buffer copy target
    mid: c_uint,      // 960x540 downscale target
    mid_fbo: c_uint,
    /// 480x270 downscale target. **Deliberately never read**, unlike `grab` and `mid`, which are
    /// both sampled as the next pass's source: this is the LAST pass, and its frame leaves through
    /// `glReadPixels` on `out_fbo`. Kept because it is the only handle on the texture object
    /// backing that fbo — dropping the name leaks it unrecoverably, with nothing to delete or
    /// re-attach it by. `#[allow]` rather than `_out` so it still reads as a live GL resource.
    #[allow(dead_code)]
    out: c_uint,
    out_fbo: c_uint,
    pending: bool,    // a frame was rendered into this chain, not yet read back
    pw: c_int,        // dims of the pending frame — LATCHED at write time (never
    ph: c_int,        //   re-derived from the live request; kills the switch race)
    read_fbo: c_uint, // which fbo holds the pending frame (mid_fbo or out_fbo)
    flip: bool,       // pending frame is bottom-up (even pass count) — encoder reverses rows
}

struct CapState {
    chains: [CapChain; 2],
    parity: usize,
    first_copy_checked: bool,
}
static mut CAPST: Option<CapState> = None;
static mut CAP_LATCHED_OFF: bool = false;

// glReadPixels time split out of the whole-cycle time (capture.rs owns that one and
// folds both into its periodic stats line). The read is the only synchronous GL call
// in the cycle — everything else is submission cost that lands at the swap.
pub(crate) static CAP_READ_US: AtomicU32 = AtomicU32::new(0);
pub(crate) static CAP_READ_N: AtomicU32 = AtomicU32::new(0);

/// A capture-ready NPOT texture: storage only (pixels NULL), refreshed via CopyTexSubImage.
/// `upload_rgba` already sets the CLAMP_TO_EDGE + LINEAR quartet these NPOT targets REQUIRE
/// (see the section comment) — same contract, one implementation.
fn cap_tex(w: c_int, h: c_int) -> c_uint {
    upload_rgba(0, w, h, std::ptr::null())
}

/// Texture + FBO pair; None (and latch-off by the caller) if incomplete. `who` names the feature
/// in the failure line — two chains build targets this way now (capture and the backdrop blur) and
/// a bare "FBO incomplete" says nothing about which one just turned itself off.
fn fbo_target(w: c_int, h: c_int, who: &str) -> Option<(c_uint, c_uint)> {
    unsafe {
        let t = cap_tex(w, h);
        let mut f: c_uint = 0;
        glGenFramebuffers(1, &mut f);
        glBindFramebuffer(GL_FRAMEBUFFER, f);
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, t, 0);
        let st = glCheckFramebufferStatus(GL_FRAMEBUFFER);
        glBindFramebuffer(GL_FRAMEBUFFER, 0);
        if st != GL_FRAMEBUFFER_COMPLETE {
            log(&format!("{who}: FBO {w}x{h} incomplete (status=0x{st:x}) — {who} off"));
            return None;
        }
        Some((t, f))
    }
}

fn cap_lazy_init() -> bool {
    unsafe {
        if (*std::ptr::addr_of!(CAPST)).is_some() {
            return true;
        }
        let mk_chain = || -> Option<CapChain> {
            let grab = cap_tex(CAP_W, CAP_H);
            let (mid, mid_fbo) = fbo_target(CAP_MID_W, CAP_MID_H, "capture")?;
            let (out, out_fbo) = fbo_target(CAP_OUT_W, CAP_OUT_H, "capture")?;
            Some(CapChain { grab, mid, mid_fbo, out, out_fbo, pending: false, pw: 0, ph: 0, read_fbo: 0, flip: false })
        };
        match (mk_chain(), mk_chain()) {
            (Some(a), Some(b)) => {
                CAPST = Some(CapState { chains: [a, b], parity: 0, first_copy_checked: false });
                true
            }
            _ => {
                CAP_LATCHED_OFF = true; // cap_target already logged the status
                false
            }
        }
    }
}

/// One capture cycle, called by `capture::tick` on the GL (main) thread between the last UI
/// draw and the swap. Writes the current back buffer (grab + downscale) into this cycle's
/// chain, and — if the *other* chain has a frame pending from the previous cycle — reads it
/// into `buf` (resized exactly). Returns `Some((w, h, flip))` when `buf` was filled.
/// `want_960` selects the 960x540 output (single pass) over the default 480x270 (two passes).
/// Returns `None` and does nothing after a completeness/copy failure (latched off).
pub(crate) fn cap_cycle(want_960: bool, buf: &mut Vec<u8>) -> Option<(c_int, c_int, bool)> {
    unsafe {
        if CAP_LATCHED_OFF || !cap_lazy_init() {
            return None;
        }
        // through a raw pointer, not `&mut CAPST` (`static_mut_refs`, a future hard error). Sound
        // for the same reason the direct form was: `cap_cycle` runs only on the GL (main) thread.
        let st = (*std::ptr::addr_of_mut!(CAPST)).as_mut().unwrap();
        let w = st.parity;
        let r = 1 - w;

        // A. grab the finished UI frame (framebuffer 0 is bound; nothing drawn after this,
        //    so the pass-flush this forces is work the swap would submit anyway).
        glBindTexture(GL_TEXTURE_2D, st.chains[w].grab);
        glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 0, 0, CAP_W, CAP_H);
        if !st.first_copy_checked {
            st.first_copy_checked = true;
            let e = glGetError();
            if e != GL_NO_ERROR {
                // e.g. INVALID_OPERATION if the window config lost its alpha bits — the copy
                // silently no-ops (black stream), so latch off loudly instead.
                log(&format!("capture: CopyTexSubImage error=0x{e:x} — capture off"));
                CAP_LATCHED_OFF = true;
                return None;
            }
        }

        // B. downscale pass(es). Blend OFF (fresh target, raw copy); glClear before each pass
        //    spares Midgard the tile preserve-load of the stale FBO contents (a full-screen
        //    quad does NOT relieve that obligation). Scissor is off here (clip pairs in-frame).
        glDisable(GL_BLEND);
        let c = &st.chains[w];
        glBindFramebuffer(GL_FRAMEBUFFER, c.mid_fbo);
        glViewport(0, 0, CAP_MID_W, CAP_MID_H);
        glClear(GL_COLOR_BUFFER_BIT);
        // full-authored-screen rect: IPROG maps (0,0,1920,1080) to the whole viewport
        draw_tex(c.grab, 0.0, 0.0, SCR_W, SCR_H, 0.0, CAP_TINT.as_ptr());
        if !want_960 {
            glBindFramebuffer(GL_FRAMEBUFFER, c.out_fbo);
            glViewport(0, 0, CAP_OUT_W, CAP_OUT_H);
            glClear(GL_COLOR_BUFFER_BIT);
            draw_tex(c.mid, 0.0, 0.0, SCR_W, SCR_H, 0.0, CAP_TINT.as_ptr());
        }
        let ch = &mut st.chains[w];
        ch.pending = true;
        if want_960 {
            ch.pw = CAP_MID_W;
            ch.ph = CAP_MID_H;
            ch.read_fbo = ch.mid_fbo;
            ch.flip = false; // 1 pass: cap (bottom-up) + 1 flip = top-down
        } else {
            ch.pw = CAP_OUT_W;
            ch.ph = CAP_OUT_H;
            ch.read_fbo = ch.out_fbo;
            ch.flip = true; // 2 passes: flips cancel = bottom-up; encoder reverses rows
        }

        // C. read the OTHER chain's frame, rendered >=1 cycle ago (complete — no drain).
        let mut got = None;
        let rc = &mut st.chains[r];
        if rc.pending {
            rc.pending = false;
            glBindFramebuffer(GL_FRAMEBUFFER, rc.read_fbo);
            glPixelStorei(GL_PACK_ALIGNMENT, 1); // RGBA rows are 4-aligned anyway; belt+braces
            let n = (rc.pw * rc.ph * 4) as usize;
            buf.resize(n, 0); // resize, NOT with_capacity: glReadPixels needs len, not capacity
            let t0 = std::time::Instant::now();
            glReadPixels(0, 0, rc.pw, rc.ph, GL_RGBA, GL_UNSIGNED_BYTE, buf.as_mut_ptr() as *mut c_void);
            CAP_READ_US.fetch_add(t0.elapsed().as_micros() as u32, Ordering::Relaxed);
            CAP_READ_N.fetch_add(1, Ordering::Relaxed);
            got = Some((rc.pw, rc.ph, rc.flip));
        }

        // D. restore the world exactly: framebuffer 0 (or every next frame renders into the
        //    FBO = frozen screen), full viewport, blend back on (func untouched). Program binding
        //    needs no restore — every draw fn binds its own lazily (use_prog); texture unit 0
        //    stays active; vertex state untouched.
        glBindFramebuffer(GL_FRAMEBUFFER, 0);
        glViewport(0, 0, CAP_W, CAP_H);
        glEnable(GL_BLEND);

        st.parity = r;
        got
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The card path's UV rect is EXACTLY the identity `vs_img.vert` used to hard-code before it
    /// took a general offset — `(a_pos - 0.5) * s + 0.5`. Graded at both ends of the quad rather
    /// than on the numbers, because "the offset is 0.5 - 0.5*s" is the claim that matters and the
    /// arithmetic is the thing that would silently drift.
    #[test]
    fn a_padded_uv_rect_still_maps_the_quad_back_onto_the_card() {
        let (w, h) = (250.0f32, 375.0f32); // a poster
        for pad in [0.0f32, 1.0, 24.0] {
            let (qw, qh) = (w + 2.0 * pad, h + 2.0 * pad);
            let uv = uv_rect_padded(w, h, qw, qh);
            let at = |a: f32, i: usize| uv[i] + a * uv[i + 2];
            // the card's own edges sit at UV 0 and 1; the shadow ring falls outside, symmetrically
            let (u0, u1) = (at(pad / qw, 0), at((pad + w) / qw, 0));
            let (v0, v1) = (at(pad / qh, 1), at((pad + h) / qh, 1));
            assert!((u0).abs() < 1e-5 && (u1 - 1.0).abs() < 1e-5, "x edges wrong at pad={pad}: {u0} {u1}");
            assert!((v0).abs() < 1e-5 && (v1 - 1.0).abs() < 1e-5, "y edges wrong at pad={pad}: {v0} {v1}");
        }
        // pad == 0 must be the identity, or every flat blit resamples itself
        assert_eq!(uv_rect_padded(w, h, w, h), [0.0, 0.0, 1.0, 1.0]);
        // a degenerate card must not divide by zero into a NaN UV (a black quad on device)
        assert!(uv_rect_padded(0.0, 0.0, 8.0, 8.0).iter().all(|v| v.is_finite()));
    }

    /// The backdrop window samples the SCREEN POSITION it is drawn at, out of a bottom-up snapshot.
    /// The v axis therefore runs backwards; getting that wrong draws the page upside down under the
    /// panel, which is the one failure that looks deliberate enough to survive a glance.
    #[test]
    fn b_blur_uv_rect_reads_the_panel_s_own_place_on_screen() {
        const FULL: [f32; 4] = [0.0, 0.0, SCR_W, SCR_H];
        const ALL: [f32; 2] = [1.0, 1.0];
        // a panel occupying the whole screen must map to the whole snapshot, either way up — and
        // this is also the identity that says the region generalised the old expression rather than
        // replacing it: whole screen grabbed, whole texture used, same four numbers as before.
        assert_eq!(blur_uv_rect(0.0, 0.0, SCR_W, SCR_H, FULL, ALL, true), [0.0, 1.0, 1.0, -1.0]);
        assert_eq!(blur_uv_rect(0.0, 0.0, SCR_W, SCR_H, FULL, ALL, false), [0.0, 0.0, 1.0, 1.0]);
        let (x, y, w, h) = (640.0f32, 240.0f32, 480.0f32, 600.0f32);
        for bottom_up in [true, false] {
            let uv = blur_uv_rect(x, y, w, h, FULL, ALL, bottom_up);
            let at = |a: f32, i: usize| uv[i] + a * uv[i + 2];
            // a_pos 0 is the panel's TOP-left in screen space, whichever way the snapshot is stored
            assert!((at(0.0, 0) - x / SCR_W).abs() < 1e-6, "left edge (bottom_up={bottom_up})");
            assert!((at(1.0, 0) - (x + w) / SCR_W).abs() < 1e-6, "right edge (bottom_up={bottom_up})");
            let (top, bot) = if bottom_up { (1.0 - y / SCR_H, 1.0 - (y + h) / SCR_H) } else { (y / SCR_H, (y + h) / SCR_H) };
            assert!((at(0.0, 1) - top).abs() < 1e-6, "top edge (bottom_up={bottom_up})");
            assert!((at(1.0, 1) - bot).abs() < 1e-6, "bottom edge (bottom_up={bottom_up})");
            assert_eq!(uv[3] < 0.0, bottom_up, "the v axis runs backwards only bottom-up");
        }
    }

    /// A REGION-limited snapshot must put the panel in the same place on screen as a full-screen one
    /// did — the whole point being that only the grab changed, never what the glass shows.
    ///
    /// The trap this pins is that there are now TWO scale factors between a screen pixel and a
    /// texel: the region is a fraction of the screen, and the region is itself only a fraction of
    /// the (still full-size) target it was rendered into. Drop either and the backdrop is offset or
    /// zoomed — visible as the glass showing something plausible that is not what is behind it.
    #[test]
    fn f_a_region_snapshot_samples_exactly_what_a_full_screen_one_did() {
        let panel = (640.0f32, 240.0f32, 480.0f32, 600.0f32);
        let (x, y, w, h) = panel;
        let reg = blur_region(x, y, w, h);
        // the region is the panel plus the reach, and the panel is strictly inside it
        assert!(reg[0] <= x - BLUR_REACH && reg[1] <= y - BLUR_REACH);
        assert!(reg[0] + reg[2] >= x + w + BLUR_REACH && reg[1] + reg[3] >= y + h + BLUR_REACH);
        // the region occupies this much of a full-size target (1920 -> 480 quarter-res, say)
        let (rw, rh) = (reg[2] / SCR_W * 480.0, reg[3] / SCR_H * 270.0);
        let span = [rw / 480.0, rh / 270.0];
        for bottom_up in [true, false] {
            let uv = blur_uv_rect(x, y, w, h, reg, span, bottom_up);
            let full = blur_uv_rect(x, y, w, h, [0.0, 0.0, SCR_W, SCR_H], [1.0, 1.0], bottom_up);
            // Both windows must cover the same FRACTION of a texel grid that has the same texel
            // SIZE — i.e. the region window is the full-screen one scaled by the region's own share
            // of the screen. Sizes first: a mismatch here is the panel showing a zoomed backdrop.
            assert!((uv[2] - full[2]).abs() < 1e-5, "u span (bottom_up={bottom_up})");
            assert!((uv[3] - full[3]).abs() < 1e-5, "v span (bottom_up={bottom_up})");
            // ...then the origin, in TEXELS of the quarter-res target, against where the panel
            // actually is inside the region. This is the half that a forgotten region offset breaks.
            let want_u = (x - reg[0]) / SCR_W * 480.0;
            assert!((uv[0] * 480.0 - want_u).abs() < 1e-3, "u origin (bottom_up={bottom_up})");
        }
    }

    /// The appear SLIDE never changes the requested region. Cached glass therefore needs no second
    /// snapshot for the slide; a dynamic policy may still refresh for source or underlay changes.
    ///
    /// A popover's first draw is at `appear == 0`, a full `rise` from where it settles. If the
    /// region were keyed on the rect being drawn, every later frame would ask for a region shifted
    /// a pixel or two and miss. `BLUR_MARGIN` carries the slide so containment holds for the whole
    /// slide; any later recapture is then a policy decision, not an accidental geometry miss.
    #[test]
    fn g_the_appear_slide_never_forces_a_second_snapshot() {
        let (x, y, w, h) = (640.0f32, 240.0f32, 480.0f32, 600.0f32);
        // grabbed once, around the panel at REST
        let reg = blur_region(x, y, w, h);
        for step in 0..=32 {
            // every frame of a rise-from-below appear, from fully displaced to settled
            let slide = POPOVER_MAX_RISE * (1.0 - step as f32 / 32.0);
            assert!(blur_region_covers(reg, x, y + slide, w, h), "missed at slide {slide}");
        }
        // and the guard that keeps that true: the grab must exceed the reach by the whole slide
        assert!(BLUR_MARGIN >= BLUR_REACH + POPOVER_MAX_RISE);
        // a panel somewhere else entirely is NOT covered — containment must still be a real test,
        // or a second panel would silently reuse the first one's backdrop
        assert!(!blur_region_covers(reg, 40.0, 40.0, w, h));
    }

    /// Two glass surfaces in one frame must converge to ONE grab, and it must SHRINK again when one
    /// of them goes away.
    ///
    /// This is the whole argument for taking the snapshot at the previous frame's union rather than
    /// at the caller's own region. Replace-on-miss (what the first version did) makes a pair of
    /// neighbouring controls ping-pong forever — each retakes the other's region every frame, two
    /// full chains, strictly worse than never having limited the grab. Growing monotonically instead
    /// fixes that and breaks the other direction: the union would keep a departed panel's area for
    /// the rest of the session, so the tab bar would go on grabbing half the screen long after the
    /// menu that needed it closed.
    ///
    /// Both halves are graded here, as the sequence the renderer actually runs.
    #[test]
    fn i_two_glass_surfaces_share_one_grab_and_give_it_back() {
        let _g = crate::testlock::serial();
        let bar = (500.0f32, 40.0, 900.0, 76.0);
        let btn = (520.0f32, 150.0, 200.0, 60.0);
        let take = |prev: [f32; 4], r: (f32, f32, f32, f32)| {
            blur_region_union(prev, blur_region(r.0, r.1, r.2, r.3))
        };

        // FRAME 1 — the button appears beside the bar. Nothing was on record, so the bar grabs its
        // own region and the button, not covered by it, has to take a second.
        let bar_only = take([0.0; 4], bar);
        assert!(!blur_region_covers(bar_only, btn.0, btn.1, btn.2, btn.3), "the pair really do not nest");
        let want = blur_region_union(bar_only, blur_region(btn.0, btn.1, btn.2, btn.3));

        // FRAME 2 — that union is what the frame ended holding, so the bar's retake takes it, and
        // the button is served by the same capture. One chain for two surfaces.
        assert!(blur_region_covers(want, bar.0, bar.1, bar.2, bar.3), "the bar is in the shared grab");
        assert!(blur_region_covers(want, btn.0, btn.1, btn.2, btn.3), "so is the button");

        // Keep neighbouring geometry from growing the shared AREA excessively. This is a geometry
        // invariant, not a timing claim: the five-pass cost still has to be measured on hardware.
        let grow = (want[2] * want[3]) / (bar_only[2] * bar_only[3]);
        assert!(grow < 1.5, "a neighbour must ride the same grab, not double it (grew {grow}x)");

        // FRAME 3 — the button is gone, so only the bar declares anything, and the region collapses
        // back. A union that only ever grew would keep the button's area for the whole session.
        let after = take([0.0; 4], bar);
        assert_eq!(after, bar_only, "the grab shrinks back the frame after a surface leaves");
    }

    /// The region in drawable px: aligned to 4, inside the canvas, never empty.
    ///
    /// Four because the chain halves twice — a size that is not a multiple of 4 does not tile back
    /// onto its source and the second reduction samples half a texel off, which shows up as the
    /// backdrop creeping sideways as a panel moves rather than as anything obviously broken.
    #[test]
    fn h_the_region_lands_on_a_4_aligned_rect_inside_the_drawable() {
        for (gw, gh) in [(1920, 1080), (960, 540)] {
            for panel in [(640.0f32, 240.0f32, 480.0f32, 600.0f32), (0.0, 0.0, 1.0, 1.0),
                          (SCR_W - 2.0, SCR_H - 2.0, 2.0, 2.0), (0.0, 0.0, SCR_W, SCR_H)] {
                let reg = blur_region(panel.0, panel.1, panel.2, panel.3);
                let (rx, ry, rw, rh) = blur_region_px(reg, gw, gh);
                assert_eq!((rw % 4, rh % 4), (0, 0), "4-aligned size ({gw}x{gh}, {panel:?})");
                assert_eq!((rx % 4, ry % 4), (0, 0), "4-aligned origin ({gw}x{gh}, {panel:?})");
                assert!(rw > 0 && rh > 0, "never empty ({gw}x{gh}, {panel:?})");
                assert!(rx >= 0 && ry >= 0 && rx + rw <= gw && ry + rh <= gh,
                    "inside the canvas ({gw}x{gh}, {panel:?}) -> {rx},{ry} {rw}x{rh}");
            }
        }
    }

    /// The chain sizes itself off the DRAWABLE, which is the bug the simulator caught: hard-coded
    /// 1920x1080 targets grabbed a rect that does not exist on a half-size surface, and every
    /// number downstream was then measured against a canvas nothing had been copied into.
    ///
    /// Graded as a PROPERTY of the reduction count rather than against literals. The first version
    /// asserted `(1920,1080) -> ((960,540),(480,270))`, which failed the moment [`BLUR_REDUCTIONS`]
    /// was turned down — correctly, and usefully, but a knob's setting is not a fact to pin.
    #[test]
    fn c_blur_dims_halve_exactly_from_whatever_the_drawable_is() {
        for (vw, vh) in [(1920, 1080), (960, 540)] {
            // the television, then the desktop simulator's half-size drawable
            let ((mw, mh), (sw, sh)) = blur_dims(vw, vh);
            assert_eq!((mw, mh), (vw / 2, vh / 2), "the first pass is always one exact halving");
            let want = (vw >> BLUR_REDUCTIONS, vh >> BLUR_REDUCTIONS);
            assert_eq!((sw, sh), want, "the tap target is {BLUR_REDUCTIONS} exact halvings down");
        }
        // odd dimensions floor rather than round up past the source (a target larger than its
        // source is a magnification pass, which is not what any of this is for)
        let ((mw, mh), (sw, sh)) = blur_dims(1919, 1079);
        assert!(mw <= 1919 / 2 && mh <= 1079 / 2 && sw <= mw && sh <= mh);
        // and a surface too small to halve must still give legal (>=1) targets, not a zero-sized
        // texture the driver reports as an incomplete FBO
        assert_eq!(blur_dims(1, 1), ((1, 1), (1, 1)));
    }

    /// The chain's shape, at whatever [`BLUR_REDUCTIONS`] is currently set to.
    ///
    /// This test used to assert the pass count was EVEN, which was true and is not a property —
    /// it was an accident of one setting of a knob that exists to be changed. What must hold is
    /// that the tap loop still lands in `a`, that the taps widen, and that the reduction count is
    /// one of the two the snapshot code actually implements.
    #[test]
    fn d_the_blur_chain_keeps_its_shape_at_either_reduction_setting() {
        assert!((1..=2).contains(&BLUR_REDUCTIONS), "blur_snapshot only builds 1 or 2 reductions");
        assert_eq!(BLUR_TAPS.len() % 2, 0, "an odd tap count leaves the snapshot in `b`, which nothing samples");
        assert!(BLUR_TAPS.windows(2).all(|w| w[1] > w[0]), "taps must WIDEN between passes");
        // The pass count must include the up pass. It did not, in two places that had each written
        // the expression out for themselves, and the disagreement inverted the lens's v axis while
        // leaving the sampling window right — a defect with no symptom on a symmetric backdrop.
        //
        assert_eq!(blur_passes(), BLUR_REDUCTIONS + BLUR_TAPS.len() + usize::from(BLUR_REDUCTIONS >= 2));
        assert_eq!(blur_is_bottom_up(), blur_passes() % 2 == 0);
    }

    /// The parity the whole chain hangs on, pinned at the CURRENT shape and tied to its reader.
    ///
    /// Anything that adds or removes a pass flips which way up the snapshot is stored, and that is
    /// the failure with no symptom: the sampling window stays right, the panel still shows the page,
    /// and what is behind the glass is simply upside down — invisible on a blurred, largely
    /// symmetric backdrop. It very nearly shipped that way twice: once when the up pass landed
    /// beside a pass count written out in two places, and again while merging the two reductions
    /// into one (a change that turned out not to be worth making — see [`blur_snapshot`] — but
    /// which did silently invert this on the way through).
    #[test]
    fn e_the_snapshots_stored_order_matches_what_reads_it() {
        // grab (bottom-up, as `glCopyTexSubImage2D` leaves it) -> reduce -> reduce -> tap -> tap -> up
        assert_eq!(blur_passes(), 5, "two reductions, two taps, one up pass");
        assert!(!blur_is_bottom_up(), "an odd pass count leaves the grab's order inverted");
        // The two readers of that parity must not be able to disagree: the sampling window and the
        // lens displacement both take it from `blur_is_bottom_up`, and a v axis that runs backwards
        // in one and forwards in the other bends the backdrop the wrong way at the rim.
        let uv = blur_uv_rect(0.0, 0.0, SCR_W, SCR_H, [0.0, 0.0, SCR_W, SCR_H], [1.0, 1.0],
            blur_is_bottom_up());
        assert!(uv[3] > 0.0, "top-down storage means the window's v span runs forwards");
    }
}
