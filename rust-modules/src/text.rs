//! SDL2_ttf text rendering (was src/text.c): font cache + glyph-texture LRU +
//! draw_text. Main-thread only (all GL), so the caches are plain statics (no
//! locking). Uses gfx's link_program/use_prog (crate path). Mostly GL/TTF FFI —
//! the retui text backend.
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::hash::{Hash, Hasher};
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::ptr::addr_of_mut;

const SCR_W: f32 = 1920.0;
const SCR_H: f32 = 1080.0;

/// Last resort only. Reaching this is a DEFECT, not a graceful degradation — see `font_at`.
const DROIDSANS: &CStr = c"/usr/share/fonts/DroidSans.ttf";

/// The shipped fonts, addressed relative to wherever the ipk actually got installed
/// (`crate::paths` explains why this cannot be a literal). Built once; `TTF_OpenFont` wants a
/// `*const c_char` that outlives the call, so these are leaked `CString`s rather than temporaries.
fn app_font(bold: bool) -> &'static CStr {
    static REG: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
    static BOLD: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
    let slot = if bold { &BOLD } else { &REG };
    slot.get_or_init(|| {
        let name = if bold { "appfont-bold.ttf" } else { "appfont.ttf" };
        CString::new(crate::paths::in_app_dir(name).into_os_string().into_encoded_bytes())
            .unwrap_or_else(|_| CString::new("/nonexistent").expect("literal has no NUL"))
    })
}

const VS_TEXT: &CStr = crate::gfx::glsl!("shaders/vs_text.vert");
const FS_TEXT: &CStr = crate::gfx::glsl!("shaders/fs_text.frag");
const FS_TEXT_FADE: &CStr = crate::gfx::glsl!("shaders/fs_text_fade.frag");
// GL enums
const GL_TEXTURE_2D: c_uint = 0x0DE1;
const GL_TRIANGLE_STRIP: c_uint = 0x0005;
const TTF_STYLE_BOLD: c_int = 0x01;
const TTF_HINTING_LIGHT: c_int = 1;

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
    /// Metrics-only sizing — the same numbers `TTF_RenderUTF8_Blended` would produce, with no
    /// rasterization and no surface. Present in the TV's own `libSDL2_ttf-2.0.so.0.14.0` (verified
    /// on device 2026-07-29 by dumping its dynamic symbols), and in the NDK sysroot copy we link
    /// against, so this resolves for real at link time like every other TTF symbol here.
    fn TTF_SizeUTF8(font: *mut TtfFont, text: *const c_char, w: *mut c_int, h: *mut c_int) -> c_int;
    fn TTF_SetFontStyle(font: *mut TtfFont, style: c_int);
    fn TTF_SetFontHinting(font: *mut TtfFont, hinting: c_int);
    fn SDL_FreeSurface(surf: *mut SdlSurface);
    // GLES2
    fn glGetUniformLocation(program: c_uint, name: *const c_char) -> c_int;
    fn glBindTexture(target: c_uint, texture: c_uint);
    fn glDeleteTextures(n: c_int, textures: *const c_uint);
    fn glUniform2f(loc: c_int, x: f32, y: f32);
    fn glUniform4fv(loc: c_int, count: c_int, value: *const f32);
    fn glUniform1i(loc: c_int, x: c_int);
    fn glUniform4f(loc: c_int, x: f32, y: f32, z: f32, w: f32);
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
    s: [u8; 96], // key PREFIX (set_entry_key truncates to 95 bytes + NUL); klen holds the full length
    hash: u64,   // key hash (FULL string+sz+bold) — a cheap pre-compare before the byte memcmp
    // Full (untruncated) key length. Load-bearing for ≥96-byte strings: the stored key is a
    // truncated prefix, so a `prefix == full_probe` equality could NEVER match — every long
    // string (a Cyrillic synopsis line crosses 95 bytes at ~48 chars) re-rendered, re-uploaded
    // and evicted a slot EVERY FRAME. The predicate is hash + klen + prefix; the 64-bit SipHash
    // over the full string is the discriminator, the prefix/len compares are the sanity check.
    klen: u32,
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
        TCacheEntry { s: [0; 96], hash: 0, klen: 0, sz: 0, bold: 0, tex: 0, w: 0, h: 0, ink_t: 0, ink_b: 0, use_: 0 };
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

/// One line per boot, not one per size — `font_at` is called for every rung of the size ladder,
/// and 70-odd identical lines would bury the rest of the log.
fn log_font_fallback_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        log(&format!(
            "FONT FALLBACK: shipped fonts missing at {} — rendering in system DroidSans; \
             the theme size ladder and the hinting contract are INVALID in this build",
            crate::paths::app_dir().display()
        ));
    });
}

unsafe fn font_at(sz: c_int, bold: c_int) -> *mut TtfFont {
    let sz = sz.clamp(8, 79) as usize;
    let arr = if bold != 0 { &mut *addr_of_mut!(FONTS_B) } else { &mut *addr_of_mut!(FONTS) };
    if arr[sz].is_null() {
        arr[sz] = TTF_OpenFont(app_font(bold != 0).as_ptr(), sz as c_int);
        if arr[sz].is_null() {
            arr[sz] = TTF_OpenFont(app_font(false).as_ptr(), sz as c_int);
            if arr[sz].is_null() {
                // DroidSans is NOT an acceptable outcome, and it used to be an invisible one: the
                // app booted looking plausible while every rung of the theme::size ladder rendered
                // in a face the light-hinting/pixel-snapping contract was never tuned for — and
                // with no bold companion, so each bold rung became synthetic emboldening applied
                // AFTER grid-fitting. `init_text` still logged `ok=1`. Measured against the shipped
                // face: -4.67% mean advance (-45.8% on `J`), +4.2% line box, 2792 -> 873 codepoints.
                // tools/font-hint-audit.py reads HOST files, so it structurally cannot catch this.
                // Say so once, loudly, so it is a reportable bug rather than a mystery.
                log_font_fallback_once();
                arr[sz] = TTF_OpenFont(DROIDSANS.as_ptr(), sz as c_int);
            }
            if !arr[sz].is_null() && bold != 0 {
                TTF_SetFontStyle(arr[sz], TTF_STYLE_BOLD);
            }
        }
        if !arr[sz].is_null() {
            // Light hinting y-snaps strokes to the NEAREST pixel. Arial's own bytecode (default
            // NORMAL hinting) rounds horizontal bars UP — at bold 26 that draws 4px bars over
            // 3px stems, inverting the design's stem>bar weight — so every size token had to
            // dodge "unlucky" px values. Light keeps the whole theme::size scale design-true.
            TTF_SetFontHinting(arr[sz], TTF_HINTING_LIGHT);
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
        TPROG = match crate::gfx::link_program(VS_TEXT.as_ptr(), FS_TEXT.as_ptr()) {
            Some(p) => p,
            None => {
                log("text prog link failed");
                return;
            }
        };
        TL_RECT = glGetUniformLocation(TPROG, c"u_trect".as_ptr());
        TL_SCREEN = glGetUniformLocation(TPROG, c"u_tscreen".as_ptr());
        TL_COL = glGetUniformLocation(TPROG, c"u_tcol".as_ptr());
        TL_TEX = glGetUniformLocation(TPROG, c"u_tex".as_ptr());
        // the fade program shares VS_TEXT; a link failure leaves TPROGF=0 and draw_text_fade
        // falls back to the plain path (the fade is a nicety, not load-bearing)
        TPROGF = crate::gfx::link_program(VS_TEXT.as_ptr(), FS_TEXT_FADE.as_ptr()).unwrap_or_else(|| {
            log("text fade prog link failed");
            0
        });
        if TPROGF != 0 {
            TLF_RECT = glGetUniformLocation(TPROGF, c"u_trect".as_ptr());
            TLF_SCREEN = glGetUniformLocation(TPROGF, c"u_tscreen".as_ptr());
            TLF_COL = glGetUniformLocation(TPROGF, c"u_tcol".as_ptr());
            TLF_TEX = glGetUniformLocation(TPROGF, c"u_tex".as_ptr());
            TLF_FADE = glGetUniformLocation(TPROGF, c"u_tfade".as_ptr());
        }
        // Constant uniforms, set once per program (per-program state): the fixed 1920x1080
        // screen and sampler unit 0. draw_text/_fade no longer re-send them per string.
        crate::gfx::use_prog(TPROG);
        glUniform2f(TL_SCREEN, SCR_W, SCR_H);
        glUniform1i(TL_TEX, 0);
        if TPROGF != 0 {
            crate::gfx::use_prog(TPROGF);
            glUniform2f(TLF_SCREEN, SCR_W, SCR_H);
            glUniform1i(TLF_TEX, 0);
        }
        if !font_at(28, 0).is_null() {
            TEXT_OK = 1;
        }
        log(&format!("init_text ok={}", TEXT_OK));
    }
}

fn entry_key(e: &TCacheEntry) -> &[u8] {
    crate::cbuf::as_bytes(&e.s)
}
fn set_entry_key(e: &mut TCacheEntry, s: &[u8]) {
    crate::cbuf::set_bytes_raw(&mut e.s, s);
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
            // klen + prefix, not full equality: the stored key is truncated to 95 bytes (see
            // TCacheEntry::klen) — starts_with degrades to equality for every short string.
            if e.hash == hash && e.tex != 0 && e.sz == sz && e.bold == bold
                && e.klen as usize == s_bytes.len() && s_bytes.starts_with(entry_key(e))
            {
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
    let tex = crate::gfx::upload_rgba(0, sw, sh, pixels as *const u8);
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
    cache[slot].klen = s_bytes.len() as u32;
    if s_bytes.len() >= 96 {
        // one-shot visibility: long keys are legal (klen disambiguates) but worth knowing about
        static mut LONG_KEY_LOGGED: bool = false;
        if !LONG_KEY_LOGGED {
            LONG_KEY_LOGGED = true;
            log(&format!("text cache: key len {} exceeds the 95-byte prefix (fine, klen-keyed)", s_bytes.len()));
        }
    }
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

/// Pixel width of `s` at `sz`/`bold` **without rasterizing anything** — pure font metrics.
///
/// This used to go through `text_tex`, i.e. it ran `TTF_RenderUTF8_Blended` (a full glyph-run
/// rasterization), scanned the resulting surface for its ink rows, uploaded a GL texture and then
/// threw all of it away to return one integer. `TTF_SizeUTF8` answers the same integer from the
/// font's glyph metrics: in SDL_ttf the blended renderer *sizes its surface with that very call*,
/// so the two widths are equal by construction, not by approximation. It also means the glyph
/// cache is no longer churned by pure measurement — the elide binary search, `TextView`'s
/// per-word wrap sweep and the HUD's template widths were each evicting real, about-to-be-drawn
/// entries out of the 160 slots to store strings nobody paints.
///
/// No caller depends on the old rasterize-as-a-side-effect: every one of them uses the number for
/// layout, and the ones that also draw the string get their texture from `draw_text` on the same
/// frame (a cache miss there, but the identical single render the measure used to do — so the
/// per-frame render count never rises, and for measure-only strings it falls to zero).
pub(crate) fn text_width(s: *const c_char, sz: c_int, bold: c_int) -> f32 {
    unsafe {
        if TEXT_OK == 0 || s.is_null() || *s == 0 {
            return 0.0;
        }
        let f = font_at(sz, bold);
        if f.is_null() {
            return 0.0;
        }
        // both out-params are real locals: SDL_ttf guards NULLs, but a height we throw away costs
        // nothing and keeps this independent of that guard surviving in the TV's 2.0.14 fork.
        let (mut w, mut h): (c_int, c_int) = (0, 0);
        if TTF_SizeUTF8(f, s, &mut w, &mut h) != 0 {
            return 0.0; // same "unmeasurable → 0" contract the null-surface path had
        }
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

/// The layout HEIGHT of one line of `sz`/`bold` text — its cap band, cap-top to baseline. Layout ≠
/// paint: descenders hang below this and are deliberately not measured (see `ui::label`'s module
/// docs for the rule). The one place this subtraction lives, because every screen that stacks text
/// needs it and three of them had written it out by hand.
pub(crate) fn cap_h(sz: c_int, bold: c_int) -> f32 {
    let (top, base) = text_cap_band(sz, bold);
    base - top
}

/// The draw-`y` (texture top) at which text of `sz`/`bold` centres its **cap band** on `cy`. Centres
/// on the font's cap-top→baseline band rather than the specific string's ink, so a label with
/// descenders ("From Beginning") and one without ("Go to Movie") land identically — descenders hang
/// below the optical centre instead of dragging the whole line up.
pub(crate) fn text_vcenter_y(sz: c_int, bold: c_int, cy: f32) -> f32 {
    let (ct, cb) = text_cap_band(sz, bold);
    cy - (ct + cb) * 0.5
}

/// align: 0 left, 1 center, 2 right (x is the anchor edge). returns text width.
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
        crate::gfx::use_prog(TPROG); // TL_SCREEN / TL_TEX / texture unit 0 set once at init
        glUniform4fv(TL_COL, 1, col);
        glBindTexture(GL_TEXTURE_2D, tex);
        // glyphs are 1:1 texel:pixel — snap the origin (see gfx::snap for the contract)
        glUniform4f(TL_RECT, crate::gfx::snap(dx), crate::gfx::snap(y), w as f32, h as f32);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
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
        crate::gfx::use_prog(TPROGF); // TLF_SCREEN / TLF_TEX / texture unit 0 set once at init
        glUniform4fv(TLF_COL, 1, col);
        // px → string-texture uv (the varying spans the one-quad string)
        let wf = w as f32;
        glUniform2f(TLF_FADE, fade_from / wf, fade_to / wf);
        glBindTexture(GL_TEXTURE_2D, tex);
        glUniform4f(TLF_RECT, crate::gfx::snap(dx), crate::gfx::snap(y), w as f32, h as f32);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        w as f32
    }
}
