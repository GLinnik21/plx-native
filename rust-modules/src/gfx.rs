//! GLES2 rendering foundation (was src/gfx.c). Three shader programs (SDF
//! rrect/tri/focus, 4-corner ambient gradient, textured RGBA), the draw primitives,
//! the spring helper, and the seven-segment FPS digits. All GLES2 calls; state is
//! main-thread statics. gfx_compile/gfx_use_base are also used by text.rs (crate path).
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicU32, Ordering};

const SCR_W: f32 = 1920.0;
const SCR_H: f32 = 1080.0;

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

const VS_SRC: &CStr = c"attribute vec2 a_pos;\nuniform vec4 u_rect;\nuniform vec2 u_screen;\nvarying vec2 v_uv;\nvoid main(){\n  v_uv = a_pos;\n  vec2 px = u_rect.xy + a_pos * u_rect.zw;\n  vec2 ndc = px / u_screen * 2.0 - 1.0;\n  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);\n}\n";
// Also carries the focus edge-sheen (`u_rimw`/`u_rimcol`, additive like the focus ring) so a rounded
// FILL can draw the 1px perimeter stroke in its own pass — same fold as FS_IMG, for the skeleton/chip
// tiles that have no texture. Disabled by `u_rimcol.a == 0` (the default from draw_rect/draw_rrect).
//
// PRECISION (all three SDF shaders — FS_SRC/FS_SHADOW/FS_IMG): the varying + every op that carries
// pixel COORDINATES must be `highp`. Midgard interpolates mediump varyings in fp16, whose error
// grows with quad size — on card-sized quads it wobbles the SDF distance by ~0.1-0.5px along a
// straight edge, dashing the 1px AA/rim rows into a "ribbed" edge (deterministic, worst on the
// resume bar's full-card quad; verified on-device by pixel-diffing captures). Texture coordinates
// on wide 1:1 quads need it too: fp16 `v_cuv` is ~1 texel off at the right of a 1920px backdrop
// (GL_LINEAR then blurs/skips texel columns), so it is highp as well — same in text.rs's `v_tuv`.
// The color path stays mediump, and the stored `d` may drop back to mediump (fp16 is exact near 0;
// the cancellation happens inside the highp sdBox).
const FS_SRC: &CStr = c"precision mediump float;\nvarying highp vec2 v_uv;\nuniform highp vec2 u_size;\nuniform highp float u_pad;\nuniform highp float u_radius;\nuniform vec4 u_colTop;\nuniform vec4 u_colBot;\nuniform float u_focus;\nuniform highp float u_radR;\nuniform float u_rimw;\nuniform vec4 u_rimcol;\nhighp float sdBox(highp vec2 p, highp vec2 b, highp float r){\n  highp vec2 q = abs(p) - b + vec2(r);\n  return length(max(q,0.0)) + min(max(q.x,q.y),0.0) - r;\n}\nvoid main(){\n  float vy = v_uv.y;\n  if (u_radius < 0.5 && u_radR < 0.5 && u_focus < 0.001) {\n    gl_FragColor = mix(u_colTop, u_colBot, vy);\n    return;\n  }\n  highp vec2 p = (v_uv - 0.5) * u_size;\n  highp vec2 hsz = u_size * 0.5 - vec2(u_pad);\n  highp float rad = (p.x > 0.0) ? u_radR : u_radius;\n  float d = sdBox(p, hsz, rad);\n  vec4 fill = mix(u_colTop, u_colBot, vy);\n  float aFill = 1.0 - smoothstep(-1.0, 1.0, d);\n  vec3 rgb = fill.rgb * aFill;\n  float a = aFill * fill.a;\n  float rim = smoothstep(-u_rimw - 0.75, -u_rimw + 0.75, d) * (1.0 - smoothstep(-0.5, 0.5, d)) * u_rimcol.a;\n  rgb += u_rimcol.rgb * rim;\n  a = max(a, rim);\n  if (u_focus > 0.001) {\n    float ring = (1.0 - smoothstep(1.5, 4.0, abs(d - 5.0))) * u_focus;\n    float glow = exp(-max(d, 0.0) / 14.0) * 0.40 * u_focus * step(0.0, d);\n    rgb += vec3(1.0) * ring + vec3(0.85, 0.9, 1.0) * glow;\n    a = max(a, max(ring, glow));\n  }\n  gl_FragColor = vec4(rgb, a);\n}\n";
const FS_AMBIENT: &CStr = c"precision mediump float;\nvarying vec2 v_uv;\nuniform vec4 u_atl, u_atr, u_abr, u_abl;\nvoid main(){\n  vec3 top = mix(u_atl.rgb, u_atr.rgb, v_uv.x);\n  vec3 bot = mix(u_abl.rgb, u_abr.rgb, v_uv.x);\n  gl_FragColor = vec4(mix(top, bot, v_uv.y), 1.0);\n}\n";
// Soft drop-shadow: an analytic SDF penumbra (one smoothstep, no gaussian/FBO). The quad is the
// card box inflated by `u_blur` on every side; `hsz` shrinks the solid core back to the card size so
// the blur band falls off OUTWARD over `u_blur` px. Its own program (like the #77 text-fade split) so
// the hot fill shader FS_SRC pays nothing. Used for the lifted-card focus shadow (ui::press replaced
// the old glow ring with soft-shadow + sheen). Circle = radius w/2.
const FS_SHADOW: &CStr = c"precision mediump float;\nvarying highp vec2 v_uv;\nuniform highp vec2 u_size;\nuniform highp float u_radius;\nuniform highp float u_blur;\nuniform highp float u_off;\nuniform vec4 u_col;\nhighp float sdBox(highp vec2 p, highp vec2 b, highp float r){ highp vec2 q=abs(p)-b+vec2(r); return length(max(q,0.0))+min(max(q.x,q.y),0.0)-r; }\nvoid main(){\n  highp vec2 p = (v_uv - 0.5) * u_size;\n  highp vec2 hsz = max(u_size*0.5 - vec2(u_blur), vec2(0.0));\n  highp vec2 pp = abs(p - vec2(0.0, -u_off));\n  if (all(lessThan(pp, hsz - vec2(u_radius + 1.0)))) discard;\n  float d = sdBox(p, hsz, min(u_radius, min(hsz.x, hsz.y)));\n  float a = (1.0 - smoothstep(-u_blur, u_blur, d)) * u_col.a;\n  gl_FragColor = vec4(u_col.rgb, a);\n}\n";
// VS hoists the two per-fragment affine terms to interpolated varyings (correct — both are affine in
// a_pos): `v_cuv` = the texture UV remapped to the inner card sub-rect (u_uvscale = quad/card size,
// 1.0 when pad==0 ⇒ v_cuv == a_pos for the flat path), `v_p` = card-local pixel coords for the SDF.
const VS_IMG: &CStr = c"attribute vec2 a_pos;\nuniform vec4 u_trect;\nuniform vec2 u_tscreen;\nuniform vec2 u_uvscale;\nvarying vec2 v_cuv;\nvarying vec2 v_p;\nvoid main(){\n  v_cuv = (a_pos - 0.5) * u_uvscale + 0.5;\n  v_p = (a_pos - 0.5) * u_trect.zw;\n  vec2 px = u_trect.xy + a_pos * u_trect.zw;\n  vec2 ndc = px / u_tscreen * 2.0 - 1.0;\n  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);\n}\n";
// The tile texture shader is the whole CARD COMPOSITE — texture + the 1px focus edge-sheen
// (`u_rimw`/`u_rimcol`) + a soft SYMMETRIC drop-shadow (`u_ch`/`u_shinv`/`u_shcol`) — all in ONE pass.
// Perf (Mali-T820, per the perf review): (1) an INTERIOR EARLY-OUT — ~85% of a card's fragments are
// strictly inside the rounded rect (`d < -2`) where rim/AA/shadow are all zero, so they skip the 4
// smoothsteps; on this per-thread tiler the branch genuinely saves the ALU. (2) UV remap + card-local
// `p` are interpolated varyings, not per-fragment math. (3) the uniform-only terms (card half-size
// `u_ch`, shadow `u_shinv = 0.5/blur`) are folded on the CPU (Midgard has no uniform pre-shader).
// (4) the 1px rim is a single-op triangle — and its width must stay ≤1px: the triangle hits exactly
// 0 at d=-2, which is what makes the d<-2 early-out seamless (a wider rim would be hard-cut there).
// The shadow `sh = smoothstep(clamp(0.5 - d/(2·blur)))` is
// algebraically identical to `1 - smoothstep(-blur, blur, d)`. `rgb = tex*m` premultiplies coverage
// (so a rounded texture's ~1px AA edge is very slightly darker under straight-alpha blend — accepted).
// Full-screen art (radius 0) takes the flat fast-path.
const FS_IMG: &CStr = c"precision mediump float;\nvarying highp vec2 v_cuv;\nvarying highp vec2 v_p;\nuniform sampler2D u_tex;\nuniform vec4 u_tint;\nuniform highp float u_iradius;\nuniform float u_rimw;\nuniform vec4 u_rimcol;\nuniform highp vec2 u_ch;\nuniform float u_shinv;\nuniform vec4 u_shcol;\nhighp float sdBox(highp vec2 p, highp vec2 b, highp float r){ highp vec2 q=abs(p)-b+vec2(r);\n  return length(max(q,0.0))+min(max(q.x,q.y),0.0)-r; }\nvoid main(){\n  vec4 c = texture2D(u_tex, v_cuv);\n  vec3 tex = c.rgb*u_tint.rgb;\n  float ta = c.a*u_tint.a;\n  if (u_iradius < 0.5) { gl_FragColor = vec4(tex, ta); return; }\n  float d = sdBox(v_p, u_ch, u_iradius);\n  if (d < -2.0) { gl_FragColor = vec4(tex, ta); return; }\n  float m = 1.0 - smoothstep(-1.0, 1.0, d);\n  float rim = max(0.0, 1.0 - abs(d + u_rimw)) * u_rimcol.a;\n  tex = mix(tex, u_rimcol.rgb, rim);\n  float sh = clamp(0.5 - d*u_shinv, 0.0, 1.0);\n  sh = sh*sh*(3.0 - 2.0*sh) * u_shcol.a * (1.0 - m);\n  gl_FragColor = vec4(tex*m, ta*m + sh);\n}\n";

const GL_VERTEX_SHADER: c_uint = 0x8B31;
const GL_FRAGMENT_SHADER: c_uint = 0x8B30;
const GL_COMPILE_STATUS: c_uint = 0x8B81;
const GL_LINK_STATUS: c_uint = 0x8B82;
const GL_ARRAY_BUFFER: c_uint = 0x8892;
const GL_STATIC_DRAW: c_uint = 0x88E4;
const GL_FLOAT: c_uint = 0x1406;
const GL_FALSE: u8 = 0;
const GL_TRIANGLE_STRIP: c_uint = 0x0005;
const GL_BLEND: c_uint = 0x0BE2;
const GL_DITHER: c_uint = 0x0BD0;
const GL_SRC_ALPHA: c_uint = 0x0302;
const GL_ONE_MINUS_SRC_ALPHA: c_uint = 0x0303;
const GL_TEXTURE_2D: c_uint = 0x0DE1;
const GL_TEXTURE0: c_uint = 0x84C0;

extern "C" {
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
    fn glBlendFunc(sfactor: c_uint, dfactor: c_uint);
    fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    fn glClear(mask: c_uint);
    fn glFinish();
    fn glGenTextures(n: c_int, textures: *mut c_uint);
    fn glDeleteTextures(n: c_int, textures: *const c_uint);
    fn glPixelStorei(pname: c_uint, param: c_int);
    fn glTexImage2D(target: c_uint, level: c_int, ifmt: c_int, w: c_int, h: c_int, border: c_int,
                    format: c_uint, ty: c_uint, pixels: *const c_void);
    fn glTexParameteri(target: c_uint, pname: c_uint, param: c_int);
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
/// GL scissor is bottom-left window pixels, but the panel is 1:1 at 1080p (no DPI scale — see
/// CLAUDE.md), so the only transform is the Y flip. Pair with [`clip_clear`]. Scrolling lists use
/// this so a partial row is cut cleanly at the frame edge instead of poking over the video / buttons
/// (`Painter` otherwise has no clip). Rect is clamped to the framebuffer so a negative edge can't
/// underflow the unsigned scissor box.
pub(crate) fn clip_set(x: f32, y: f32, w: f32, h: f32) {
    let x0 = x.max(0.0);
    let y_top = y.max(0.0);
    let x1 = (x + w).min(SCR_W);
    let y1 = (y + h).min(SCR_H);
    let (wi, hi) = ((x1 - x0).max(0.0), (y1 - y_top).max(0.0));
    unsafe {
        glEnable(GL_SCISSOR_TEST);
        // GL y is bottom-up: the box's bottom in GL space is SCR_H - (top + height)
        glScissor(x0 as c_int, (SCR_H - (y_top + hi)) as c_int, wi as c_int, hi as c_int);
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

pub(crate) fn gfx_compile(ty: c_uint, src: *const c_char) -> c_uint {
    unsafe {
        let s = glCreateShader(ty);
        glShaderSource(s, 1, &src, std::ptr::null());
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

pub(crate) fn gfx_use_base() {
    unsafe { glUseProgram(PROG) };
}

static QUAD: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];

pub(crate) fn init_gl() {
    unsafe {
        PROG = glCreateProgram();
        glAttachShader(PROG, gfx_compile(GL_VERTEX_SHADER, VS_SRC.as_ptr()));
        glAttachShader(PROG, gfx_compile(GL_FRAGMENT_SHADER, FS_SRC.as_ptr()));
        glBindAttribLocation(PROG, 0, c"a_pos".as_ptr());
        glLinkProgram(PROG);
        let mut ok: c_int = 0;
        glGetProgramiv(PROG, GL_LINK_STATUS, &mut ok);
        if ok == 0 {
            eprintln!("link failed");
            std::process::exit(1);
        }
        glUseProgram(PROG);
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

        APROG = glCreateProgram();
        glAttachShader(APROG, gfx_compile(GL_VERTEX_SHADER, VS_SRC.as_ptr()));
        glAttachShader(APROG, gfx_compile(GL_FRAGMENT_SHADER, FS_AMBIENT.as_ptr()));
        glBindAttribLocation(APROG, 0, c"a_pos".as_ptr());
        glLinkProgram(APROG);
        AL_RECT = glGetUniformLocation(APROG, c"u_rect".as_ptr());
        AL_SCREEN = glGetUniformLocation(APROG, c"u_screen".as_ptr());
        AL_TL = glGetUniformLocation(APROG, c"u_atl".as_ptr());
        AL_TR = glGetUniformLocation(APROG, c"u_atr".as_ptr());
        AL_BR = glGetUniformLocation(APROG, c"u_abr".as_ptr());
        AL_BL = glGetUniformLocation(APROG, c"u_abl".as_ptr());

        // Soft-shadow program (own program so the hot FS_SRC pays nothing; mirrors init_image).
        SPROG = glCreateProgram();
        glAttachShader(SPROG, gfx_compile(GL_VERTEX_SHADER, VS_SRC.as_ptr()));
        glAttachShader(SPROG, gfx_compile(GL_FRAGMENT_SHADER, FS_SHADOW.as_ptr()));
        glBindAttribLocation(SPROG, 0, c"a_pos".as_ptr());
        glLinkProgram(SPROG);
        glGetProgramiv(SPROG, GL_LINK_STATUS, &mut ok);
        if ok == 0 {
            log("shadow prog link failed");
            SPROG = 0; // draw_shadow no-ops → cards simply lose the drop-shadow, nothing else breaks
        } else {
            SL_RECT = glGetUniformLocation(SPROG, c"u_rect".as_ptr());
            SL_SCREEN = glGetUniformLocation(SPROG, c"u_screen".as_ptr());
            SL_SIZE = glGetUniformLocation(SPROG, c"u_size".as_ptr());
            SL_RADIUS = glGetUniformLocation(SPROG, c"u_radius".as_ptr());
            SL_BLUR = glGetUniformLocation(SPROG, c"u_blur".as_ptr());
            SL_OFF = glGetUniformLocation(SPROG, c"u_off".as_ptr());
            SL_COL = glGetUniformLocation(SPROG, c"u_col".as_ptr());
        }

        glUseProgram(PROG);

        glEnable(GL_BLEND);
        glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        // GL_DITHER is ON by default in GLES2; it dithers low-alpha gradients (the card shadow
        // penumbra) into a regular ordered-dither dot pattern visible along tile edges. The panel is
        // 888 and SURFACE_APP is snapped to exact 8-bit codes, so dithering buys nothing here — off.
        glDisable(GL_DITHER);
    }
}

pub(crate) fn draw_rect(x: f32, y: f32, w: f32, h: f32, pad: f32, radius: f32, top: *const f32, bot: *const f32, focus: f32) {
    unsafe {
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

pub(crate) fn draw_ambient(x: f32, y: f32, w: f32, h: f32, dim: f32, tl: *const f32, tr: *const f32, br: *const f32, bl: *const f32) {
    unsafe {
        let c3 = |p: *const f32, i: usize| *p.add(i);
        glUseProgram(APROG);
        glUniform2f(AL_SCREEN, SCR_W, SCR_H);
        glUniform4f(AL_RECT, x, y, w, h);
        glUniform4f(AL_TL, c3(tl, 0) * dim, c3(tl, 1) * dim, c3(tl, 2) * dim, 1.0);
        glUniform4f(AL_TR, c3(tr, 0) * dim, c3(tr, 1) * dim, c3(tr, 2) * dim, 1.0);
        glUniform4f(AL_BR, c3(br, 0) * dim, c3(br, 1) * dim, c3(br, 2) * dim, 1.0);
        glUniform4f(AL_BL, c3(bl, 0) * dim, c3(bl, 1) * dim, c3(bl, 2) * dim, 1.0);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        glUseProgram(PROG);
    }
}

pub(crate) fn draw_rrect(x: f32, y: f32, w: f32, h: f32, rad_l: f32, rad_r: f32, col: *const f32) {
    unsafe {
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
/// into `y`. No-ops if the program failed to link. Own GL program, so it doesn't disturb the base
/// shader's uniforms (restores `PROG` after).
pub(crate) fn draw_shadow(x: f32, y: f32, w: f32, h: f32, radius: f32, blur: f32, off: f32, col: *const f32) {
    unsafe {
        if SPROG == 0 {
            return;
        }
        let b = blur.max(0.5);
        let (qx, qy, qw, qh) = (x - b, y - b, w + 2.0 * b, h + 2.0 * b);
        glUseProgram(SPROG);
        glUniform2f(SL_SCREEN, SCR_W, SCR_H);
        glUniform4f(SL_RECT, qx, qy, qw, qh);
        glUniform2f(SL_SIZE, qw, qh);
        glUniform1f(SL_RADIUS, radius);
        glUniform1f(SL_BLUR, b);
        glUniform1f(SL_OFF, off); // occluder (tile) offset above the shadow box; shader discards the covered interior
        glUniform4fv(SL_COL, 1, col);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        glUseProgram(PROG);
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
        IPROG = glCreateProgram();
        glAttachShader(IPROG, gfx_compile(GL_VERTEX_SHADER, VS_IMG.as_ptr()));
        glAttachShader(IPROG, gfx_compile(GL_FRAGMENT_SHADER, FS_IMG.as_ptr()));
        glBindAttribLocation(IPROG, 0, c"a_pos".as_ptr());
        glLinkProgram(IPROG);
        let mut ok: c_int = 0;
        glGetProgramiv(IPROG, GL_LINK_STATUS, &mut ok);
        if ok == 0 {
            log("image prog link failed");
            return;
        }
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
        glUseProgram(PROG);
    }
}

/// Upload a straight-alpha RGBA8 bitmap (`w`×`h`, tightly packed) into a GL texture. Reuses
/// `prev` if non-zero (re-specs it), else allocates a new id. Returns the texture id. Used for
/// image-subtitle (PGS/VobSub) overlays, which change only every few seconds. Main-thread only.
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
        glUseProgram(IPROG);
        glUniform2f(IL_SCREEN, SCR_W, SCR_H);
        glUniform4fv(IL_TINT, 1, tint);
        glUniform2f(IL_UVSCALE, uvsx, uvsy);
        glUniform1f(IL_RADIUS, radius);
        glUniform1f(IL_RIMW, rimw);
        glUniform4fv(IL_RIMCOL, 1, rimcol);
        glUniform2f(IL_CH, w * 0.5, h * 0.5);
        glUniform1f(IL_SHINV, shinv);
        glUniform4fv(IL_SHCOL, 1, shcol);
        glActiveTexture(GL_TEXTURE0);
        glBindTexture(GL_TEXTURE_2D, tex);
        glUniform1i(IL_TEX, 0);
        glUniform4f(IL_RECT, qx, qy, qw, qh);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        glUseProgram(PROG);
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
