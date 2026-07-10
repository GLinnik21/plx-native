//! SDL2_ttf text rendering (was src/text.c): font cache + glyph-texture LRU +
//! draw_text. Main-thread only (all GL), so the caches are plain statics (no
//! locking). Uses gfx's gfx_compile/gfx_use_base (crate path). Mostly GL/TTF FFI —
//! the retui text backend.
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::ptr::addr_of_mut;

const SCR_W: f32 = 1920.0;
const SCR_H: f32 = 1080.0;

const APP_FONT: &CStr = c"/media/developer/apps/usr/palm/applications/com.glin.plexpoc/appfont.ttf";
const APP_FONT_BOLD: &CStr = c"/media/developer/apps/usr/palm/applications/com.glin.plexpoc/appfont-bold.ttf";
const DROIDSANS: &CStr = c"/usr/share/fonts/DroidSans.ttf";

const VS_TEXT: &CStr = c"attribute vec2 a_pos;\nuniform vec4 u_trect;\nuniform vec2 u_tscreen;\nvarying vec2 v_tuv;\nvoid main(){ v_tuv=a_pos; vec2 px=u_trect.xy+a_pos*u_trect.zw;\n  vec2 ndc=px/u_tscreen*2.0-1.0; gl_Position=vec4(ndc.x,-ndc.y,0.0,1.0); }\n";
const FS_TEXT: &CStr = c"precision mediump float;\nvarying vec2 v_tuv;\nuniform sampler2D u_tex;\nuniform vec4 u_tcol;\nvoid main(){ float a=texture2D(u_tex,v_tuv).a; gl_FragColor=vec4(u_tcol.rgb, u_tcol.a*a); }\n";

// GL enums
const GL_VERTEX_SHADER: c_uint = 0x8B31;
const GL_FRAGMENT_SHADER: c_uint = 0x8B30;
const GL_LINK_STATUS: c_uint = 0x8B82;
const GL_TEXTURE_2D: c_uint = 0x0DE1;
const GL_RGBA: c_uint = 0x1908;
const GL_UNSIGNED_BYTE: c_uint = 0x1401;
const GL_UNPACK_ALIGNMENT: c_uint = 0x0CF5;
const GL_TEXTURE_MIN_FILTER: c_uint = 0x2801;
const GL_TEXTURE_MAG_FILTER: c_uint = 0x2800;
const GL_TEXTURE_WRAP_S: c_uint = 0x2802;
const GL_TEXTURE_WRAP_T: c_uint = 0x2803;
const GL_LINEAR: c_int = 0x2601;
const GL_CLAMP_TO_EDGE: c_int = 0x812F;
const GL_TRIANGLE_STRIP: c_uint = 0x0005;
const GL_TEXTURE0: c_uint = 0x84C0;
const TTF_STYLE_BOLD: c_int = 0x01;

enum TtfFont {}

#[repr(C)]
struct SdlSurface {
    flags: u32,
    format: *mut c_void,
    w: c_int,
    h: c_int,
    pitch: c_int,
    pixels: *mut c_void,
    // remaining fields (userdata, clip_rect, ...) unused
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SdlColor {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

extern "C" {
    fn TTF_Init() -> c_int;
    fn TTF_OpenFont(file: *const c_char, ptsize: c_int) -> *mut TtfFont;
    fn TTF_RenderUTF8_Blended(font: *mut TtfFont, text: *const c_char, fg: SdlColor) -> *mut SdlSurface;
    fn TTF_SetFontStyle(font: *mut TtfFont, style: c_int);
    fn SDL_FreeSurface(surf: *mut SdlSurface);
    // GLES2
    fn glCreateProgram() -> c_uint;
    fn glAttachShader(program: c_uint, shader: c_uint);
    fn glBindAttribLocation(program: c_uint, index: c_uint, name: *const c_char);
    fn glLinkProgram(program: c_uint);
    fn glGetProgramiv(program: c_uint, pname: c_uint, params: *mut c_int);
    fn glGetUniformLocation(program: c_uint, name: *const c_char) -> c_int;
    fn glGenTextures(n: c_int, textures: *mut c_uint);
    fn glBindTexture(target: c_uint, texture: c_uint);
    fn glPixelStorei(pname: c_uint, param: c_int);
    fn glTexImage2D(target: c_uint, level: c_int, ifmt: c_int, w: c_int, h: c_int, border: c_int,
                    format: c_uint, ty: c_uint, pixels: *const c_void);
    fn glTexParameteri(target: c_uint, pname: c_uint, param: c_int);
    fn glDeleteTextures(n: c_int, textures: *const c_uint);
    fn glUseProgram(program: c_uint);
    fn glUniform2f(loc: c_int, x: f32, y: f32);
    fn glUniform4fv(loc: c_int, count: c_int, value: *const f32);
    fn glUniform1i(loc: c_int, x: c_int);
    fn glUniform4f(loc: c_int, x: f32, y: f32, z: f32, w: f32);
    fn glActiveTexture(texture: c_uint);
    fn glDrawArrays(mode: c_uint, first: c_int, count: c_int);
}

static mut TPROG: c_uint = 0;
static mut TL_RECT: c_int = 0;
static mut TL_SCREEN: c_int = 0;
static mut TL_COL: c_int = 0;
static mut TL_TEX: c_int = 0;
static mut FONTS: [*mut TtfFont; 80] = [std::ptr::null_mut(); 80];
static mut FONTS_B: [*mut TtfFont; 80] = [std::ptr::null_mut(); 80];
static mut TEXT_OK: c_int = 0;

#[derive(Clone, Copy)]
struct TCacheEntry {
    s: [u8; 96],
    sz: c_int,
    bold: c_int,
    tex: c_uint,
    w: c_int,
    h: c_int,
    use_: c_uint,
}
impl TCacheEntry {
    const ZERO: TCacheEntry = TCacheEntry { s: [0; 96], sz: 0, bold: 0, tex: 0, w: 0, h: 0, use_: 0 };
}
const TCACHE: usize = 48;
static mut TCACHE_A: [TCacheEntry; TCACHE] = [TCacheEntry::ZERO; TCACHE];
static mut TCLOCK: c_uint = 0;

fn log(m: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/poc-events.log") {
        let _ = writeln!(f, "{m}");
    }
}

unsafe fn font_at(sz: c_int, bold: c_int) -> *mut TtfFont {
    let sz = sz.clamp(8, 79) as usize;
    let arr = if bold != 0 { &mut *addr_of_mut!(FONTS_B) } else { &mut *addr_of_mut!(FONTS) };
    if arr[sz].is_null() {
        let path = if bold != 0 { APP_FONT_BOLD } else { APP_FONT };
        arr[sz] = TTF_OpenFont(path.as_ptr(), sz as c_int);
        if arr[sz].is_null() {
            arr[sz] = TTF_OpenFont(APP_FONT.as_ptr(), sz as c_int);
            if arr[sz].is_null() {
                arr[sz] = TTF_OpenFont(DROIDSANS.as_ptr(), sz as c_int);
            }
            if !arr[sz].is_null() && bold != 0 {
                TTF_SetFontStyle(arr[sz], TTF_STYLE_BOLD);
            }
        }
    }
    arr[sz]
}

pub(crate) fn init_text() {
    unsafe {
        if TTF_Init() != 0 {
            log("TTF_Init failed");
            return;
        }
        TPROG = glCreateProgram();
        glAttachShader(TPROG, crate::gfx::gfx_compile(GL_VERTEX_SHADER, VS_TEXT.as_ptr()));
        glAttachShader(TPROG, crate::gfx::gfx_compile(GL_FRAGMENT_SHADER, FS_TEXT.as_ptr()));
        glBindAttribLocation(TPROG, 0, c"a_pos".as_ptr());
        glLinkProgram(TPROG);
        let mut ok: c_int = 0;
        glGetProgramiv(TPROG, GL_LINK_STATUS, &mut ok);
        if ok == 0 {
            log("text prog link failed");
            return;
        }
        TL_RECT = glGetUniformLocation(TPROG, c"u_trect".as_ptr());
        TL_SCREEN = glGetUniformLocation(TPROG, c"u_tscreen".as_ptr());
        TL_COL = glGetUniformLocation(TPROG, c"u_tcol".as_ptr());
        TL_TEX = glGetUniformLocation(TPROG, c"u_tex".as_ptr());
        if !font_at(28, 0).is_null() {
            TEXT_OK = 1;
        }
        crate::gfx::gfx_use_base();
        log(&format!("init_text ok={}", TEXT_OK));
    }
}

fn entry_key(e: &TCacheEntry) -> &[u8] {
    let n = e.s.iter().position(|&b| b == 0).unwrap_or(e.s.len());
    &e.s[..n]
}
fn set_entry_key(e: &mut TCacheEntry, s: &[u8]) {
    e.s = [0; 96];
    let n = s.len().min(e.s.len() - 1);
    e.s[..n].copy_from_slice(&s[..n]);
}

/// returns (GL texture id (0 on failure), w, h)
unsafe fn text_tex(s_bytes: &[u8], s_c: *const c_char, sz: c_int, bold: c_int) -> (c_uint, c_int, c_int) {
    {
        let cache = &mut *addr_of_mut!(TCACHE_A);
        for e in cache.iter_mut() {
            if e.tex != 0 && e.sz == sz && e.bold == bold && entry_key(e) == s_bytes {
                TCLOCK = TCLOCK.wrapping_add(1);
                e.use_ = TCLOCK;
                return (e.tex, e.w, e.h);
            }
        }
    }
    let f = font_at(sz, bold);
    if f.is_null() {
        return (0, 0, 0);
    }
    let white = SdlColor { r: 255, g: 255, b: 255, a: 255 };
    let surf = TTF_RenderUTF8_Blended(f, s_c, white);
    if surf.is_null() {
        return (0, 0, 0);
    }
    let (sw, sh, pixels) = ((*surf).w, (*surf).h, (*surf).pixels);
    let mut tex: c_uint = 0;
    glGenTextures(1, &mut tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA as c_int, sw, sh, 0, GL_RGBA, GL_UNSIGNED_BYTE, pixels);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    SDL_FreeSurface(surf);

    let cache = &mut *addr_of_mut!(TCACHE_A);
    let mut slot = 0usize;
    let mut oldest = c_uint::MAX;
    for i in 0..TCACHE {
        if cache[i].tex == 0 {
            slot = i;
            break;
        }
        if cache[i].use_ < oldest {
            oldest = cache[i].use_;
            slot = i;
        }
    }
    if cache[slot].tex != 0 {
        glDeleteTextures(1, &cache[slot].tex);
    }
    set_entry_key(&mut cache[slot], s_bytes);
    cache[slot].sz = sz;
    cache[slot].bold = bold;
    cache[slot].tex = tex;
    cache[slot].w = sw;
    cache[slot].h = sh;
    TCLOCK = TCLOCK.wrapping_add(1);
    cache[slot].use_ = TCLOCK;
    (tex, sw, sh)
}

/// align: 0 left, 1 center, 2 right (x is the anchor edge). returns text width.
/// pixel width of `s` at `sz`/`bold` without drawing (renders+caches the glyph texture, like
/// draw_text, then returns just its width). Used for eliding long labels to a budget.
pub(crate) fn text_width(s: *const c_char, sz: c_int, bold: c_int) -> f32 {
    unsafe {
        if TEXT_OK == 0 || s.is_null() {
            return 0.0;
        }
        let cs = CStr::from_ptr(s);
        let b = cs.to_bytes();
        if b.is_empty() {
            return 0.0;
        }
        let (_tex, w, _h) = text_tex(b, s, sz, bold);
        w as f32
    }
}

/// rendered height in px of a line of text at `sz`/`bold` — the font's line height, independent of
/// the string. Used to vertically center a glyph on a text line (e.g. the transport clock).
pub(crate) fn text_height(sz: c_int, bold: c_int) -> f32 {
    unsafe {
        if TEXT_OK == 0 {
            return sz as f32;
        }
        let (_tex, _w, h) = text_tex(b"0", c"0".as_ptr(), sz, bold);
        h as f32
    }
}

pub(crate) fn draw_text(s: *const c_char, x: f32, y: f32, sz: c_int, col: *const f32, align: c_int, bold: c_int) -> f32 {
    unsafe {
        if TEXT_OK == 0 || s.is_null() {
            return 0.0;
        }
        let cs = CStr::from_ptr(s);
        let s_bytes = cs.to_bytes();
        if s_bytes.is_empty() {
            return 0.0;
        }
        let (tex, w, h) = text_tex(s_bytes, s, sz, bold);
        if tex == 0 {
            return 0.0;
        }
        let dx = match align {
            1 => x - w as f32 * 0.5,
            2 => x - w as f32,
            _ => x,
        };
        glUseProgram(TPROG);
        glUniform2f(TL_SCREEN, SCR_W, SCR_H);
        glUniform4fv(TL_COL, 1, col);
        glActiveTexture(GL_TEXTURE0);
        glBindTexture(GL_TEXTURE_2D, tex);
        glUniform1i(TL_TEX, 0);
        glUniform4f(TL_RECT, dx, y, w as f32, h as f32);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        crate::gfx::gfx_use_base(); // restore rect program for subsequent draw_rect
        w as f32
    }
}
