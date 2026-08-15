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
    fn glFinish();
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
    let s = crate::surface::scale();
    let (vx, vy, _, _) = crate::surface::viewport();
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
pub(crate) fn clip_clear() {
    unsafe { glDisable(GL_SCISSOR_TEST) }
}

/// clear the framebuffer to an opaque color — the retui frame's first op, so the
/// framework doesn't have to link GLES itself (it draws only through gfx/text).
pub(crate) fn frame_clear(r: f32, g: f32, b: f32) {
    unsafe {
        glClearColor(r, g, b, 1.0);
        glClear(GL_COLOR_BUFFER_BIT);
    }
}

/// Block until the GPU has finished all queued commands. Used ONLY by the draw profiler
/// (`ui::profile`) to attribute per-phase GPU cost — it serializes the pipeline, so never call it
/// on the normal render path.
pub(crate) fn gl_finish() {
    unsafe { glFinish() }
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
static mut IL_UVSCALE: c_int = 0;
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

pub(crate) fn draw_rect(x: f32, y: f32, w: f32, h: f32, pad: f32, radius: f32, top: *const f32, bot: *const f32, focus: f32) {
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
        glUniform1f(LOC_RIMW, 0.0);
        glUniform4f(LOC_RIMCOL, 0.0, 0.0, 0.0, 0.0); // no edge-sheen (default)
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
}

/// [`draw_rect`] with the focus edge-sheen (a `rimw`-px inset perimeter rim in `rimcol`) baked into
/// the same fill pass — the no-texture (skeleton / chip disc) counterpart of [`draw_tex_stroked`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_rect_sheened(x: f32, y: f32, w: f32, h: f32, radius: f32, top: *const f32, bot: *const f32, rimw: f32, rimcol: *const f32) {
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
const SEG: [u8; 10] = [0x3F, 0x06, 0x5B, 0x4F, 0x66, 0x6D, 0x7D, 0x07, 0x7F, 0x6F];

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
        draw_rect(sx, sy, sw, sh, 2.0, (w + 4.0) / 2.0 - 2.0, col, col, 0.0);
    }
}

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
        IL_UVSCALE = glGetUniformLocation(IPROG, c"u_uvscale".as_ptr());
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
#[allow(clippy::too_many_arguments)]
fn draw_tex_impl(tex: c_uint, x: f32, y: f32, w: f32, h: f32, radius: f32, tint: *const f32, rimw: f32, rimcol: *const f32,
    pad: f32, shblur: f32, shcol: *const f32) {
    if tex == 0 {
        return;
    }
    unsafe {
        let (qx, qy, qw, qh) = (x - pad, y - pad, w + 2.0 * pad, h + 2.0 * pad); // inflate for the penumbra
        // CPU-fold the uniform-only terms (Midgard has no uniform pre-shader): card half-size, the
        // quad→card UV scale (1.0 when pad==0), and the shadow's 0.5/blur normaliser.
        let uvsx = if w > 0.0 { qw / w } else { 1.0 };
        let uvsy = if h > 0.0 { qh / h } else { 1.0 };
        let shinv = if shblur > 0.0 { 0.5 / shblur } else { 0.0 };
        use_prog(IPROG); // IL_SCREEN / IL_TEX / texture unit 0 are set once at init
        glUniform4fv(IL_TINT, 1, tint);
        glUniform2f(IL_UVSCALE, uvsx, uvsy);
        glUniform1f(IL_RADIUS, radius);
        glUniform1f(IL_RIMW, rimw);
        glUniform4fv(IL_RIMCOL, 1, rimcol);
        glUniform2f(IL_CH, w * 0.5, h * 0.5);
        glUniform1f(IL_SHINV, shinv);
        glUniform4fv(IL_SHCOL, 1, shcol);
        glBindTexture(GL_TEXTURE_2D, tex);
        glUniform4f(IL_RECT, qx, qy, qw, qh);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
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
    CARD_CT.fetch_add(1, Ordering::Relaxed);
    // the inflated (shadow) quad crossing a screen edge ⇒ some shadow fragments are drawn off-screen
    // (viewport-clipped, but still rasterized). Counts partial+full; fully-off-screen ⇒ a cull miss.
    if x - pad < 0.0 || y - pad < 0.0 || x + w + pad > SCR_W || y + h + pad > SCR_H {
        CARD_OFF.fetch_add(1, Ordering::Relaxed);
    }
    draw_tex_impl(tex, x, y, w, h, radius, tint, rimw, rimcol, pad, shblur, shcol);
}

use crate::log;

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
    out: c_uint,      // 480x270 downscale target
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

/// Texture + FBO pair; None (and latch-off by the caller) if incomplete.
fn cap_target(w: c_int, h: c_int) -> Option<(c_uint, c_uint)> {
    unsafe {
        let t = cap_tex(w, h);
        let mut f: c_uint = 0;
        glGenFramebuffers(1, &mut f);
        glBindFramebuffer(GL_FRAMEBUFFER, f);
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, t, 0);
        let st = glCheckFramebufferStatus(GL_FRAMEBUFFER);
        glBindFramebuffer(GL_FRAMEBUFFER, 0);
        if st != GL_FRAMEBUFFER_COMPLETE {
            log(&format!("capture: FBO {w}x{h} incomplete (status=0x{st:x}) — capture off"));
            return None;
        }
        Some((t, f))
    }
}

fn cap_lazy_init() -> bool {
    unsafe {
        if CAPST.is_some() {
            return true;
        }
        let mk_chain = || -> Option<CapChain> {
            let grab = cap_tex(CAP_W, CAP_H);
            let (mid, mid_fbo) = cap_target(CAP_MID_W, CAP_MID_H)?;
            let (out, out_fbo) = cap_target(CAP_OUT_W, CAP_OUT_H)?;
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
        let st = CAPST.as_mut().unwrap();
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
