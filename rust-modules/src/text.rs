//! SDL2_ttf text rendering (was src/text.c): font cache + glyph-texture LRU +
//! draw_text. Main-thread only (all GL), so the caches are plain statics (no
//! locking). Uses gfx's gfx_compile/gfx_use_base (crate path). Mostly GL/TTF FFI —
//! the retui text backend.
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::hash::{Hash, Hasher};
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::ptr::addr_of_mut;

const SCR_W: f32 = 1920.0;
const SCR_H: f32 = 1080.0;

const APP_FONT: &CStr = c"/media/developer/apps/usr/palm/applications/com.glin.plexpoc/appfont.ttf";
const APP_FONT_BOLD: &CStr = c"/media/developer/apps/usr/palm/applications/com.glin.plexpoc/appfont-bold.ttf";
const DROIDSANS: &CStr = c"/usr/share/fonts/DroidSans.ttf";

const VS_TEXT: &CStr = c"attribute vec2 a_pos;\nuniform vec4 u_trect;\nuniform vec2 u_tscreen;\nvarying vec2 v_tuv;\nvoid main(){ v_tuv=a_pos; vec2 px=u_trect.xy+a_pos*u_trect.zw;\n  vec2 ndc=px/u_tscreen*2.0-1.0; gl_Position=vec4(ndc.x,-ndc.y,0.0,1.0); }\n";
const FS_TEXT: &CStr = c"precision mediump float;\nvarying vec2 v_tuv;\nuniform sampler2D u_tex;\nuniform vec4 u_tcol;\nvoid main(){ float a=texture2D(u_tex,v_tuv).a; gl_FragColor=vec4(u_tcol.rgb, u_tcol.a*a); }\n";
// The FADE variant is a SEPARATE program bound only by draw_text_fade (one caller — the About
// card's MORE): u_tfade = (from, to) in string-texture uv.x fades the glyph alpha 1→0 across that
// band. Kept off the shared FS_TEXT so every ordinary glyph doesn't pay the per-fragment smoothstep
// on this fill-rate-bound panel.
const FS_TEXT_FADE: &CStr = c"precision mediump float;\nvarying vec2 v_tuv;\nuniform sampler2D u_tex;\nuniform vec4 u_tcol;\nuniform vec2 u_tfade;\nvoid main(){ float a=texture2D(u_tex,v_tuv).a;\n  a *= 1.0 - smoothstep(u_tfade.x, u_tfade.y, v_tuv.x);\n  gl_FragColor=vec4(u_tcol.rgb, u_tcol.a*a); }\n";

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
// the fade program (draw_text_fade only) + its own uniform locations
static mut TPROGF: c_uint = 0;
static mut TLF_RECT: c_int = 0;
static mut TLF_SCREEN: c_int = 0;
static mut TLF_COL: c_int = 0;
static mut TLF_TEX: c_int = 0;
static mut TLF_FADE: c_int = 0;
static mut FONTS: [*mut TtfFont; 80] = [std::ptr::null_mut(); 80];
static mut FONTS_B: [*mut TtfFont; 80] = [std::ptr::null_mut(); 80];
static mut TEXT_OK: c_int = 0;

#[derive(Clone, Copy)]
struct TCacheEntry {
    s: [u8; 96],
    hash: u64, // key hash (string+sz+bold) — a cheap pre-compare before the byte memcmp
    sz: c_int,
    bold: c_int,
    tex: c_uint,
    w: c_int,
    h: c_int,
    // vertical *ink* bounds within the h-tall texture: the first/last rows that actually contain
    // visible glyph pixels. Lets callers centre by what's drawn, not the font's line box.
    ink_t: c_int,
    ink_b: c_int,
    use_: c_uint,
}
impl TCacheEntry {
    const ZERO: TCacheEntry =
        TCacheEntry { s: [0; 96], hash: 0, sz: 0, bold: 0, tex: 0, w: 0, h: 0, ink_t: 0, ink_b: 0, use_: 0 };
}

/// the cache key hash: string bytes + size + bold. The per-frame lookup is a linear scan of all
/// 160 slots (~80-100 text ops/frame on the detail page); comparing one u64 first makes the
/// non-matching 159 probes a single integer compare instead of a size/bold/byte-key memcmp each.
fn key_hash(s: &[u8], sz: c_int, bold: c_int) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    sz.hash(&mut h);
    bold.hash(&mut h);
    h.finish()
}
// Big enough to hold every distinct string visible in one frame WITHOUT eviction. The detail About
// panel (~30 runs) + Cast row (~14) + episodes/related/hero titles push well past ~48; at that point
// an LRU thrashes — each frame re-renders dozens of lines via TTF (+ a full-surface ink scan) and
// re-uploads them, which showed up as ~22ms About / ~12ms Cast (sub-30fps scrolling into them). With
// headroom past the ~80 simultaneous worst case, stable text is a pure cache hit after first paint.
const TCACHE: usize = 160;
static mut TCACHE_A: [TCacheEntry; TCACHE] = [TCacheEntry::ZERO; TCACHE];
static mut TCLOCK: c_uint = 0;

use crate::log;

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
        // the fade program shares VS_TEXT; a link failure leaves TPROGF=0 and draw_text_fade
        // falls back to the plain path (the fade is a nicety, not load-bearing)
        TPROGF = glCreateProgram();
        glAttachShader(TPROGF, crate::gfx::gfx_compile(GL_VERTEX_SHADER, VS_TEXT.as_ptr()));
        glAttachShader(TPROGF, crate::gfx::gfx_compile(GL_FRAGMENT_SHADER, FS_TEXT_FADE.as_ptr()));
        glBindAttribLocation(TPROGF, 0, c"a_pos".as_ptr());
        glLinkProgram(TPROGF);
        let mut okf: c_int = 0;
        glGetProgramiv(TPROGF, GL_LINK_STATUS, &mut okf);
        if okf == 0 {
            log("text fade prog link failed");
            TPROGF = 0;
        } else {
            TLF_RECT = glGetUniformLocation(TPROGF, c"u_trect".as_ptr());
            TLF_SCREEN = glGetUniformLocation(TPROGF, c"u_tscreen".as_ptr());
            TLF_COL = glGetUniformLocation(TPROGF, c"u_tcol".as_ptr());
            TLF_TEX = glGetUniformLocation(TPROGF, c"u_tex".as_ptr());
            TLF_FADE = glGetUniformLocation(TPROGF, c"u_tfade".as_ptr());
        }
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

/// scan a freshly-rendered TTF surface for its vertical ink bounds — the first and last rows that
/// hold a visible (non-transparent) pixel. Blended text is white-on-clear, so alpha (byte 3 of each
/// ARGB8888 pixel) is the coverage. An all-blank string (e.g. a space) reports the full box.
unsafe fn surface_ink_v(surf: *const SdlSurface) -> (c_int, c_int) {
    let (w, h, pitch) = ((*surf).w, (*surf).h, (*surf).pitch as isize);
    let base = (*surf).pixels as *const u8;
    if base.is_null() || w <= 0 || h <= 0 {
        return (0, h.max(0));
    }
    let (mut top, mut bot) = (h, -1);
    for y in 0..h {
        let row = base.offset(y as isize * pitch);
        let mut ink = false;
        for x in 0..w {
            if *row.offset(x as isize * 4 + 3) > 24 {
                ink = true;
                break;
            }
        }
        if ink {
            if y < top {
                top = y;
            }
            bot = y;
        }
    }
    if bot < 0 {
        (0, h)
    } else {
        (top, bot + 1)
    }
}

/// returns (GL texture id (0 on failure), w, h, ink_top, ink_bottom)
unsafe fn text_tex(s_bytes: &[u8], s_c: *const c_char, sz: c_int, bold: c_int) -> (c_uint, c_int, c_int, c_int, c_int) {
    let hash = key_hash(s_bytes, sz, bold);
    {
        let cache = &mut *addr_of_mut!(TCACHE_A);
        for e in cache.iter_mut() {
            if e.hash == hash && e.tex != 0 && e.sz == sz && e.bold == bold && entry_key(e) == s_bytes {
                TCLOCK = TCLOCK.wrapping_add(1);
                e.use_ = TCLOCK;
                return (e.tex, e.w, e.h, e.ink_t, e.ink_b);
            }
        }
    }
    let f = font_at(sz, bold);
    if f.is_null() {
        return (0, 0, 0, 0, 0);
    }
    let white = SdlColor { r: 255, g: 255, b: 255, a: 255 };
    let surf = TTF_RenderUTF8_Blended(f, s_c, white);
    if surf.is_null() {
        return (0, 0, 0, 0, 0);
    }
    let (sw, sh, pixels) = ((*surf).w, (*surf).h, (*surf).pixels);
    let (ink_t, ink_b) = surface_ink_v(surf);
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
    cache[slot].hash = hash;
    cache[slot].sz = sz;
    cache[slot].bold = bold;
    cache[slot].tex = tex;
    cache[slot].w = sw;
    cache[slot].h = sh;
    cache[slot].ink_t = ink_t;
    cache[slot].ink_b = ink_b;
    TCLOCK = TCLOCK.wrapping_add(1);
    cache[slot].use_ = TCLOCK;
    (tex, sw, sh, ink_t, ink_b)
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
        let (_tex, w, _h, _it, _ib) = text_tex(b, s, sz, bold);
        w as f32
    }
}

static mut ELIDE_CACHE: Option<HashMap<u64, String>> = None;

/// Truncate `s` to fit `budget` px at `sz`/`bold`, ellipsised — and **memoised**. The per-frame
/// binary search over candidate widths (each candidate a `text_width` call) is the text-measure
/// thrash that dropped the player Info panel to ~1fps; caching the RESULT by (text, budget, sz, bold,
/// cont) makes every later frame a hash lookup, thrash-proof no matter how long the input. `cont=false`
/// returns `s` unchanged when it already fits (a plain elide — track menu / chapters / info card);
/// `cont=true` always marks the result with `…` (a continued line whose caller already knows more text
/// follows — `TextView`). The single truncation impl for the whole UI. Main-thread only, like the
/// glyph cache.
pub(crate) fn elide(s: &str, budget: f32, sz: c_int, bold: c_int, cont: bool) -> String {
    let mut hh = DefaultHasher::new();
    s.hash(&mut hh);
    (budget as i32).hash(&mut hh);
    sz.hash(&mut hh);
    bold.hash(&mut hh);
    cont.hash(&mut hh);
    let key = hh.finish();
    let cache = unsafe { (*addr_of_mut!(ELIDE_CACHE)).get_or_insert_with(HashMap::new) };
    if let Some(v) = cache.get(&key) {
        return v.clone();
    }
    let out = elide_compute(s, budget, sz, bold, cont);
    if cache.len() > 512 {
        cache.clear(); // crude cap — plenty for every label/line across a few screens
    }
    cache.insert(key, out.clone());
    out
}

fn elide_compute(s: &str, budget: f32, sz: c_int, bold: c_int, cont: bool) -> String {
    let measure = |t: &str| CString::new(t).ok().map(|c| text_width(c.as_ptr(), sz, bold)).unwrap_or(0.0);
    let target = if cont { format!("{s}\u{2026}") } else { s.to_string() };
    if budget <= 0.0 || measure(&target) <= budget {
        return target;
    }
    // largest char-prefix of `s` whose "prefix…" still fits `budget`
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cand = chars[..mid].iter().collect::<String>().trim_end().to_string() + "\u{2026}";
        if measure(&cand) <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect::<String>().trim_end().to_string() + "\u{2026}"
}

/// rendered height in px of a line of text at `sz`/`bold` — the font's line height, independent of
/// the string. Used to vertically center a glyph on a text line (e.g. the transport clock).
pub(crate) fn text_height(sz: c_int, bold: c_int) -> f32 {
    unsafe {
        if TEXT_OK == 0 {
            return sz as f32;
        }
        let (_tex, _w, h, _it, _ib) = text_tex(b"0", c"0".as_ptr(), sz, bold);
        h as f32
    }
}

/// The font's **cap band** at `sz`/`bold`: (cap_top, baseline) offsets from the draw-y, measured
/// once from a reference capital ("H", which is flat-topped and sits on the baseline). This is the
/// stable, string-independent band UI toolkits centre type on — it deliberately ignores descenders
/// (g j y p q) and ascenders, so every label of a given size aligns the same way. Falls back to a
/// rough em band without TTF.
pub(crate) fn text_cap_band(sz: c_int, bold: c_int) -> (f32, f32) {
    unsafe {
        if TEXT_OK == 0 {
            return (sz as f32 * 0.15, sz as f32 * 0.9);
        }
        let (_t, _w, _h, it, ib) = text_tex(b"H", c"H".as_ptr(), sz, bold);
        (it as f32, ib as f32)
    }
}

/// The draw-`y` (texture top) at which text of `sz`/`bold` centres its **cap band** on `cy`. Centres
/// on the font's cap-top→baseline band rather than the specific string's ink, so a label with
/// descenders ("From Beginning") and one without ("Go to Movie") land identically — descenders hang
/// below the optical centre instead of dragging the whole line up.
pub(crate) fn text_vcenter_y(sz: c_int, bold: c_int, cy: f32) -> f32 {
    let (ct, cb) = text_cap_band(sz, bold);
    cy - (ct + cb) * 0.5
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
        let (tex, w, h, _it, _ib) = text_tex(s_bytes, s, sz, bold);
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

/// [`draw_text`] with a horizontal fade-out (its OWN GL program, so plain text pays no per-fragment
/// fade cost): glyph alpha runs 1→0 between `fade_from`..`fade_to` px from the string's LEFT edge
/// (regardless of `align`). Used for a truncated line that must dissolve before an overlapping
/// affordance (the About card's MORE label). Falls back to plain [`draw_text`] if the fade program
/// failed to link.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_text_fade(
    s: *const c_char, x: f32, y: f32, sz: c_int, col: *const f32, align: c_int, bold: c_int,
    fade_from: f32, fade_to: f32,
) -> f32 {
    unsafe {
        if TPROGF == 0 {
            return draw_text(s, x, y, sz, col, align, bold);
        }
        if TEXT_OK == 0 || s.is_null() {
            return 0.0;
        }
        let cs = CStr::from_ptr(s);
        let s_bytes = cs.to_bytes();
        if s_bytes.is_empty() {
            return 0.0;
        }
        let (tex, w, h, _it, _ib) = text_tex(s_bytes, s, sz, bold);
        if tex == 0 {
            return 0.0;
        }
        let dx = match align {
            1 => x - w as f32 * 0.5,
            2 => x - w as f32,
            _ => x,
        };
        glUseProgram(TPROGF);
        glUniform2f(TLF_SCREEN, SCR_W, SCR_H);
        glUniform4fv(TLF_COL, 1, col);
        // px → string-texture uv (the varying spans the one-quad string)
        let wf = w as f32;
        glUniform2f(TLF_FADE, fade_from / wf, fade_to / wf);
        glActiveTexture(GL_TEXTURE0);
        glBindTexture(GL_TEXTURE_2D, tex);
        glUniform1i(TLF_TEX, 0);
        glUniform4f(TLF_RECT, dx, y, w as f32, h as f32);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        crate::gfx::gfx_use_base(); // restore rect program for subsequent draw_rect
        w as f32
    }
}
