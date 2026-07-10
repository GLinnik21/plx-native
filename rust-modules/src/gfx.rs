//! GLES2 rendering foundation (was src/gfx.c). Three shader programs (SDF
//! rrect/tri/focus, 4-corner ambient gradient, textured RGBA), the draw primitives,
//! the spring helper, and the seven-segment FPS digits. All GLES2 calls; state is
//! main-thread statics. gfx_compile/gfx_use_base are also used by text.rs (crate path).
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};

const SCR_W: f32 = 1920.0;
const SCR_H: f32 = 1080.0;

const VS_SRC: &CStr = c"attribute vec2 a_pos;\nuniform vec4 u_rect;\nuniform vec2 u_screen;\nvarying vec2 v_uv;\nvoid main(){\n  v_uv = a_pos;\n  vec2 px = u_rect.xy + a_pos * u_rect.zw;\n  vec2 ndc = px / u_screen * 2.0 - 1.0;\n  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);\n}\n";
const FS_SRC: &CStr = c"precision mediump float;\nvarying vec2 v_uv;\nuniform vec2 u_size;\nuniform float u_pad;\nuniform float u_radius;\nuniform vec4 u_colTop;\nuniform vec4 u_colBot;\nuniform float u_focus;\nuniform float u_shape;\nuniform float u_radR;\nfloat sdBox(vec2 p, vec2 b, float r){\n  vec2 q = abs(p) - b + vec2(r);\n  return length(max(q,0.0)) + min(max(q.x,q.y),0.0) - r;\n}\nvoid main(){\n  if (u_shape > 0.5) {\n    float tri = step(0.5*v_uv.x, v_uv.y) * step(v_uv.y, 1.0 - 0.5*v_uv.x);\n    gl_FragColor = vec4(u_colTop.rgb * tri, tri * u_colTop.a);\n    return;\n  }\n  vec2 p = (v_uv - 0.5) * u_size;\n  vec2 hsz = u_size * 0.5 - vec2(u_pad);\n  float rad = (p.x > 0.0) ? u_radR : u_radius;\n  float d = sdBox(p, hsz, rad);\n  vec4 fill = mix(u_colTop, u_colBot, v_uv.y);\n  float aFill = 1.0 - smoothstep(-1.0, 1.0, d);\n  vec3 rgb = fill.rgb * aFill;\n  float a = aFill * fill.a;\n  if (u_focus > 0.001) {\n    float ring = (1.0 - smoothstep(1.5, 4.0, abs(d - 5.0))) * u_focus;\n    float glow = exp(-max(d, 0.0) / 14.0) * 0.40 * u_focus * step(0.0, d);\n    rgb += vec3(1.0) * ring + vec3(0.85, 0.9, 1.0) * glow;\n    a = max(a, max(ring, glow));\n  }\n  gl_FragColor = vec4(rgb, a);\n}\n";
const FS_AMBIENT: &CStr = c"precision mediump float;\nvarying vec2 v_uv;\nuniform vec4 u_atl, u_atr, u_abr, u_abl;\nvoid main(){\n  vec3 top = mix(u_atl.rgb, u_atr.rgb, v_uv.x);\n  vec3 bot = mix(u_abl.rgb, u_abr.rgb, v_uv.x);\n  gl_FragColor = vec4(mix(top, bot, v_uv.y), 1.0);\n}\n";
const VS_IMG: &CStr = c"attribute vec2 a_pos;\nuniform vec4 u_trect;\nuniform vec2 u_tscreen;\nvarying vec2 v_tuv;\nvoid main(){ v_tuv=a_pos; vec2 px=u_trect.xy+a_pos*u_trect.zw;\n  vec2 ndc=px/u_tscreen*2.0-1.0; gl_Position=vec4(ndc.x,-ndc.y,0.0,1.0); }\n";
const FS_IMG: &CStr = c"precision mediump float;\nvarying vec2 v_tuv;\nuniform sampler2D u_tex;\nuniform vec4 u_tint;\nuniform vec2 u_isize;\nuniform float u_iradius;\nfloat sdBox(vec2 p, vec2 b, float r){ vec2 q=abs(p)-b+vec2(r);\n  return length(max(q,0.0))+min(max(q.x,q.y),0.0)-r; }\nvoid main(){\n  vec4 c = texture2D(u_tex, v_tuv);\n  vec2 p = (v_tuv-0.5)*u_isize;\n  float d = sdBox(p, u_isize*0.5, u_iradius);\n  float m = 1.0 - smoothstep(-1.0, 1.0, d);\n  gl_FragColor = vec4(c.rgb*u_tint.rgb, c.a*u_tint.a*m);\n}\n";

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
    fn glBlendFunc(sfactor: c_uint, dfactor: c_uint);
    fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    fn glClear(mask: c_uint);
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

/// clear the framebuffer to an opaque color — the retui frame's first op, so the
/// framework doesn't have to link GLES itself (it draws only through gfx/text).
pub(crate) fn frame_clear(r: f32, g: f32, b: f32) {
    unsafe {
        glClearColor(r, g, b, 1.0);
        glClear(GL_COLOR_BUFFER_BIT);
    }
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
static mut LOC_SHAPE: c_int = 0;
static mut LOC_RADR: c_int = 0;

static mut APROG: c_uint = 0;
static mut AL_RECT: c_int = 0;
static mut AL_SCREEN: c_int = 0;
static mut AL_TL: c_int = 0;
static mut AL_TR: c_int = 0;
static mut AL_BR: c_int = 0;
static mut AL_BL: c_int = 0;

static mut IPROG: c_uint = 0;
static mut IL_RECT: c_int = 0;
static mut IL_SCREEN: c_int = 0;
static mut IL_TINT: c_int = 0;
static mut IL_SIZE: c_int = 0;
static mut IL_RADIUS: c_int = 0;
static mut IL_TEX: c_int = 0;

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
        LOC_SHAPE = glGetUniformLocation(PROG, c"u_shape".as_ptr());
        LOC_RADR = glGetUniformLocation(PROG, c"u_radR".as_ptr());
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
        glUseProgram(PROG);

        glEnable(GL_BLEND);
        glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    }
}

pub(crate) fn draw_rect(x: f32, y: f32, w: f32, h: f32, pad: f32, radius: f32, top: *const f32, bot: *const f32, focus: f32) {
    unsafe {
        glUniform4f(LOC_RECT, x, y, w, h);
        glUniform2f(LOC_SIZE, w, h);
        glUniform1f(LOC_PAD, pad);
        glUniform1f(LOC_RADIUS, radius);
        glUniform1f(LOC_RADR, radius);
        glUniform4fv(LOC_COLTOP, 1, top);
        glUniform4fv(LOC_COLBOT, 1, bot);
        glUniform1f(LOC_FOCUS, focus);
        glUniform1f(LOC_SHAPE, 0.0);
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
        glUniform4f(LOC_RECT, x, y, w, h);
        glUniform2f(LOC_SIZE, w, h);
        glUniform1f(LOC_PAD, 0.0);
        glUniform1f(LOC_RADIUS, rad_l);
        glUniform1f(LOC_RADR, rad_r);
        glUniform4fv(LOC_COLTOP, 1, col);
        glUniform4fv(LOC_COLBOT, 1, col);
        glUniform1f(LOC_FOCUS, 0.0);
        glUniform1f(LOC_SHAPE, 0.0);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
}

pub(crate) fn draw_ptri(x: f32, y: f32, w: f32, h: f32, col: *const f32) {
    unsafe {
        glUniform4f(LOC_RECT, x, y, w, h);
        glUniform2f(LOC_SIZE, w, h);
        glUniform4fv(LOC_COLTOP, 1, col);
        glUniform4fv(LOC_COLBOT, 1, col);
        glUniform1f(LOC_FOCUS, 0.0);
        glUniform1f(LOC_SHAPE, 1.0);
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
        IL_SIZE = glGetUniformLocation(IPROG, c"u_isize".as_ptr());
        IL_RADIUS = glGetUniformLocation(IPROG, c"u_iradius".as_ptr());
        IL_TEX = glGetUniformLocation(IPROG, c"u_tex".as_ptr());
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

/// draw texture in px rect (x,y,w,h), rounded corners `radius`, multiplied by tint.
pub(crate) fn draw_tex(tex: c_uint, x: f32, y: f32, w: f32, h: f32, radius: f32, tint: *const f32) {
    if tex == 0 {
        return;
    }
    unsafe {
        glUseProgram(IPROG);
        glUniform2f(IL_SCREEN, SCR_W, SCR_H);
        glUniform4fv(IL_TINT, 1, tint);
        glUniform2f(IL_SIZE, w, h);
        glUniform1f(IL_RADIUS, radius);
        glActiveTexture(GL_TEXTURE0);
        glBindTexture(GL_TEXTURE_2D, tex);
        glUniform1i(IL_TEX, 0);
        glUniform4f(IL_RECT, x, y, w, h);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        glUseProgram(PROG);
    }
}

fn log(m: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/poc-events.log") {
        let _ = writeln!(f, "{m}");
    }
}
