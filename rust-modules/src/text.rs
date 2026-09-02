//! SDL2_ttf text rendering (was src/text.c): font cache + glyph-texture LRU +
//! draw_text. Main-thread only (all GL), so the caches are plain statics (no
//! locking). Uses gfx's link_program/use_prog (crate path). Mostly GL/TTF FFI —
//! the retui text backend.
//!
//! # The fallback chain, and why the unit of work is a RUN
//!
//! Inter carries 2853 codepoints — Latin, Cyrillic, Greek — and no Hangul, Kana or Han at all, so
//! a Korean, Japanese or Chinese Plex library used to render as tofu end to end. SDL2_ttf 2.0.x
//! has **no fallback-font API**: one `TTF_Font` is one face, and `TTF_RenderUTF8_Blended` draws
//! whatever that one face has, box glyphs included. So the chain has to be built here, and
//! building it means changing the unit of work from a *string* to a **run**:
//!
//!   1. split the string into maximal runs, each entirely drawable by ONE face
//!      ([`split_runs`], deciding per character through [`crate::fontcov`]),
//!   2. render each run with its own face,
//!   3. composite the run surfaces side by side, **aligned on the baseline**, into one buffer,
//!   4. upload that buffer exactly as a single-run string was uploaded before.
//!
//! Step 4 is what keeps everything downstream unchanged: the 160-slot LRU still keys on
//! `(string bytes, size, bold)` — run splitting is a pure function of the string, so the key still
//! determines the texture — and the `ink_t`/`ink_b` cap-band machinery still measures one texture.
//!
//! **The chain, in order:** Inter (`appfont.ttf`) → the **bundled** `appfont-cjk.ttf` → the
//! television's own `/usr/share/fonts/DroidSansFallback.ttf`. The bundled face is the guarantee,
//! because a submission is graded on whatever firmware the reviewer happens to have; the system
//! face behind it is free insurance, measured present on webOS 4.10.0 (2026-08-23: 11172/11172
//! Hangul, 20902 Han, Kana, and — see below — no Hebrew and no Arabic).
//!
//! **Baseline, not top.** Two faces have two line boxes, so compositing by the surface top would
//! shift Latin down whenever a CJK glyph shared the line. Every run is placed so its baseline
//! lands on the BASE face's baseline (`TTF_RenderUTF8_Blended` puts the baseline `TTF_FontAscent`
//! rows from the top), and the composite keeps the base face's box. That box provably contains the
//! fallback's ink at every rung of `theme::size`: measured host-side through FreeType with
//! SDL_ttf's own metric arithmetic, Noto Sans CJK's ink top is 16..53 px against Inter's 22..70 px
//! ascent for 22..72 px sizes, and its descent is shallower at every rung. Swept across the whole
//! Hangul / Han / Kana / fullwidth repertoire the fallback's ink reaches 0.842–0.939 em above the
//! baseline against Inter's 0.9688 em ascent, so the `y < 0` guard in [`render_runs`] discards only
//! blank rows. The single exception in the font is **U+3031** (a vertical-writing kana repeat mark,
//! 1.323 em), which SDL_ttf already clips inside its own run surface before the compositor sees it.
//!
//! One consequence of anchoring on the base face, worth knowing before anyone reports it as a bug:
//! `text_cap_band` still measures Inter's "H", so a CJK label vertically centred through
//! [`text_vcenter_y`] is centred on INTER's cap band, not on its own ink. CJK glyphs fill more of
//! the em than Latin caps do (at 28 px: ink 21 px above the baseline against Inter's 15), so such a
//! label sits ~1–2 px lower than optically centred. That is deliberate, not an oversight — the cap
//! band is string-INDEPENDENT by design (see [`text_cap_band`]), which is what makes every label of
//! a size align with every other, and making it depend on the script would break that for the mixed
//! lines this feature exists to draw.
//!
//! **The fallback face is NOT emboldened for bold runs.** It ships at one weight (a second is
//! ~21 MB), and SDL_ttf's synthetic bold is `FT_Outline_Embolden` at ppem/10 — 2.2 px at
//! `size::MICRO`. Rendered against this face that fills 電視劇, 曇天 and 鬱 into solid blocks at
//! **every** size in the ladder, 22 through 40. A weight mismatch beside bold Latin is a
//! typographic compromise; an illegible ideograph is a defect, so bold runs use the regular
//! fallback face.
//!
//! **RTL is out of scope.** No link of the chain has Hebrew or Arabic, and that is deliberate
//! twice over: coverage is not support. Arabic needs joining and contextual shaping, Hebrew needs
//! bidi reordering, and neither exists anywhere in this module — `TTF_RenderUTF8_Blended` in
//! SDL2_ttf 2.0.x is a left-to-right advance loop with no shaper behind it. Adding a face without
//! a shaper would turn tofu into fluent-looking WRONG text, which is harder to notice and worse to
//! ship. `fontcov`'s `rtl_is_out_of_scope_and_stays_that_way` asserts the absence so this stays a
//! decision rather than an oversight.
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::hash::{Hash, Hasher};
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::ptr::{addr_of, addr_of_mut};

use crate::surface::{LOGICAL_H as SCR_H, LOGICAL_W as SCR_W};
use crate::ui::overdraw::{gate, Class};

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
        let name = if bold {
            "appfont-bold.ttf"
        } else {
            "appfont.ttf"
        };
        CString::new(
            crate::paths::in_app_dir(name)
                .into_os_string()
                .into_encoded_bytes(),
        )
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
    fn TTF_RenderUTF8_Blended(
        font: *mut TtfFont,
        text: *const c_char,
        fg: SdlColor,
    ) -> *mut SdlSurface;
    /// Metrics-only sizing — the same numbers `TTF_RenderUTF8_Blended` would produce, with no
    /// rasterization and no surface. Present in the TV's own `libSDL2_ttf-2.0.so.0.14.0` (verified
    /// on device 2026-07-29 by dumping its dynamic symbols), and in the NDK sysroot copy we link
    /// against, so this resolves for real at link time like every other TTF symbol here.
    fn TTF_SizeUTF8(font: *mut TtfFont, text: *const c_char, w: *mut c_int, h: *mut c_int)
        -> c_int;
    /// Rows from the top of a `TTF_RenderUTF8_Blended` surface to the BASELINE — the anchor the
    /// run compositor aligns every face on. `TTF_FontHeight` is that surface's height. Both are
    /// plain metric readers with no allocation.
    ///
    /// **Declared here rather than routed through `dynlib!` because the SONAME holds still**, which
    /// is that module's actual criterion — `libSDL2_ttf-2.0.so.0` on all 14 firmware inventories
    /// (`tools/fwcompat.py --inventory libSDL2_ttf`), unlike libcurl's `.so.5`→`.so.4`. Symbol
    /// presence is the second half, also 14/14 (`--lib libSDL2_ttf-2.0.so.0 --grep '^TTF_FontAscent$'`).
    ///
    /// **READ THIS BEFORE ADDING ANOTHER `TTF_*` SYMBOL.** For this one library, "it compiles and
    /// links" is evidence of NOTHING. The repo pins SDL2's *core* headers in `include/` but not
    /// SDL_ttf's, so a TTF call compiles against the NDK's header and links against the NDK's
    /// `libSDL2_ttf-2.0.so.0.18.0` — **2.0.18**, while every firmware ships 2.0.10 or 2.0.14. The
    /// NDK exports 75 `TTF_*` functions; the televisions export 47 or 48, and **27 of them link
    /// cleanly here and exist on no supported release** (`TTF_SetFontSize`, `TTF_MeasureUTF8`,
    /// `TTF_SetDirection`, every `*32` and `*_Wrapped`, …). There is no local signal at all: it
    /// builds, `make check` passes, and the process dies at the first call on every set. Grade a
    /// new symbol with `tools/fwcompat.py --lib libSDL2_ttf-2.0.so.0 --grep '^NAME$'` before
    /// writing the call.
    ///
    /// That is exactly the trap this module dodged. The coverage query one would reach for —
    /// `TTF_GlyphIsProvided` — takes a `Uint16`, so it cannot be asked about anything above
    /// U+FFFF; and `TTF_GlyphIsProvided32` is in the NDK header, exported by the NDK `.so`, and
    /// present on **none of the 14** (`--grep '32$'` returns zero matches on every release). So
    /// `crate::fontcov` reads the cmap from the file instead, which also makes coverage a host
    /// question rather than a device one.
    fn TTF_FontAscent(font: *mut TtfFont) -> c_int;
    fn TTF_FontHeight(font: *mut TtfFont) -> c_int;
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
/// **Vertical edge-fade bands, in logical screen y** (the same absolute space `u_trect`'s `y`
/// lives in) — the counterpart of [`TLF_FADE`]'s horizontal one. `(0.0, 0.0)` (the default, since
/// `y1 > y0` is then false) means "off": the shader's gate skips the multiply rather than relying
/// on a sentinel width like the horizontal band's, because a vertical band has no natural "past
/// the string" edge to sentinel against. See [`draw_text_fade`] and `ui::widgets::edge_feather`'s
/// replacement note in `ui::person_bio` for why this exists — a scrolling viewport's edge used to
/// be an OPAQUE panel-coloured gradient painted over the glass, which read as a grey band rather
/// than the text dissolving.
static mut TLF_VTOP: c_int = 0;
static mut TLF_VBOT: c_int = 0;
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
    const ZERO: TCacheEntry = TCacheEntry {
        s: [0; 96],
        hash: 0,
        klen: 0,
        sz: 0,
        bold: 0,
        tex: 0,
        w: 0,
        h: 0,
        ink_t: 0,
        ink_b: 0,
        use_: 0,
    };
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
    let arr = if bold != 0 {
        &mut *addr_of_mut!(FONTS_B)
    } else {
        &mut *addr_of_mut!(FONTS)
    };
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

// ------------------------------------------------------------------------------------------------
// The fallback chain. See this module's header for the design; this half is the mechanism.
// ------------------------------------------------------------------------------------------------

/// One link of the chain, in priority order. `as usize` indexes [`COV`] and the face arrays, so
/// the discriminants are load-bearing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Link {
    /// Inter — `appfont.ttf` / `appfont-bold.ttf`. The app's typeface, and the ONLY link with a
    /// bold companion. 2853 codepoints.
    Base = 0,
    /// The bundled `appfont-cjk.ttf` (Noto Sans CJK KR, `tools/cut-noto-cjk.py`). 44810
    /// codepoints: all of Hangul, Kana, Han and CJK punctuation. Shipped rather than borrowed
    /// because a submission is graded on whichever firmware the reviewer has.
    Cjk = 1,
    /// The television's own `DroidSansFallback.ttf`. Free insurance behind the bundled face —
    /// measured on webOS 4.10.0 as covering the same Hangul/Han/Kana ground — and it costs
    /// nothing when it is absent, as it is on the desktop simulator and the macOS app.
    ///
    /// **That measurement is ONE television and cannot be generalised.** `tools/fwcompat.py`
    /// grades symbols, not filesystems, so it has nothing to say about a font PATH on the other 13
    /// releases; we have no images for them. This is precisely why link 2 is bundled rather than
    /// borrowed — this link is a bonus that may or may not exist, and the code treats it as such.
    Sys = 2,
}
const CHAIN: [Link; 3] = [Link::Base, Link::Cjk, Link::Sys];

/// The bundled fallback face, cut by `tools/cut-noto-cjk.py` and staged beside the binary.
const CJK_FONT: &str = "appfont-cjk.ttf";
/// The television's pan-CJK fallback. Distinct from [`DROIDSANS`], which is the *Latin* face and a
/// defect path: reaching this one is normal and expected on a Korean library.
const DROIDSANS_FALLBACK: &str = "/usr/share/fonts/DroidSansFallback.ttf";

static mut FONTS_CJK: [*mut TtfFont; 80] = [std::ptr::null_mut(); 80];
static mut FONTS_SYS: [*mut TtfFont; 80] = [std::ptr::null_mut(); 80];
/// Per-link coverage, read from the file's cmap on FIRST NEED and never again. `None` after
/// `COV_TRIED` means the file is absent or unreadable, i.e. the link is empty.
static mut COV: [Option<crate::fontcov::Coverage>; 3] = [None, None, None];
static mut COV_TRIED: [bool; 3] = [false; 3];

/// The ONE place a link's file is named — both the coverage read and the `TTF_OpenFont` go
/// through it, so the two can never disagree about which file a link is.
fn link_file(link: Link) -> std::path::PathBuf {
    match link {
        // Coverage is read from the REGULAR face only. That is sound because `fontcov`'s
        // `bold_face_covers_exactly_what_the_regular_one_does` asserts the two are identical in
        // `make check` — the gate is not decoration here, it is what makes one read enough.
        Link::Base => crate::paths::in_app_dir("appfont.ttf"),
        Link::Cjk => crate::paths::in_app_dir(CJK_FONT),
        Link::Sys => std::path::PathBuf::from(DROIDSANS_FALLBACK),
    }
}

/// Can `link` draw `cp`? Loads that link's coverage on first ask.
///
/// The BASE link answers `true` for everything when its own cmap cannot be read. That is
/// deliberate: an install whose `appfont.ttf` is missing has already fallen through to
/// `font_at`'s DroidSans branch and logged loudly, and in that state the useful behaviour is the
/// pre-chain one — render the whole string in one call with whatever face opened — not to split
/// every string into runs against a coverage set of nothing.
unsafe fn link_covers(link: Link, cp: u32) -> bool {
    let i = link as usize;
    let tried = &mut *addr_of_mut!(COV_TRIED);
    if !tried[i] {
        tried[i] = true;
        let path = link_file(link);
        let cov = &mut *addr_of_mut!(COV);
        match crate::fontcov::of_file(&path) {
            Ok(c) => {
                log(&format!(
                    "font chain: {link:?} {} codepoints={}",
                    path.display(),
                    c.len()
                ));
                cov[i] = Some(c);
            }
            // Not a failure for Cjk/Sys — a television without DroidSansFallback, or a host
            // simulator with no bundled face, simply has a shorter chain. Logged once either way,
            // WITH the path, because "why is this still tofu" is otherwise unanswerable from a log
            // and the answer is usually that the file is not where the install put it.
            Err(e) => log(&format!(
                "font chain: {link:?} unavailable — {}: {e}",
                path.display()
            )),
        }
    }
    match &(*addr_of!(COV))[i] {
        Some(c) => c.contains(cp),
        None => link == Link::Base,
    }
}

/// The face for one run. Bold exists only on [`Link::Base`]: the fallback ships at a single
/// weight, and SDL_ttf's synthetic bold turns dense ideographs into solid blocks at every UI size
/// (measured — see this module's header). So a bold CJK run renders in the regular fallback face,
/// which is also what halves the memory below.
///
/// **Cost, and why it is lazy.** SDL_ttf 2.0.x opens one `FT_Face` per (file, ptsize) — there is
/// no size sharing — and this face has 65535 glyphs, so its `loca`, `hmtx` and `cmap` are large.
/// Host-measured through the same FreeType: **~2.1 MB resident per opened size**, ~17 MB if a
/// library exercises all eight rungs of `theme::size`. Nothing here opens until a string actually
/// needs it, so a Latin or Cyrillic library pays exactly zero — and sharing one face between
/// regular and bold is what keeps the worst case at eight faces rather than sixteen.
unsafe fn link_font(link: Link, sz: c_int, bold: c_int) -> *mut TtfFont {
    if link == Link::Base {
        return font_at(sz, bold);
    }
    let sz = sz.clamp(8, 79) as usize;
    let arr = match link {
        Link::Cjk => &mut *addr_of_mut!(FONTS_CJK),
        _ => &mut *addr_of_mut!(FONTS_SYS),
    };
    if arr[sz].is_null() {
        let file = link_file(link);
        let Ok(c) = CString::new(file.into_os_string().into_encoded_bytes()) else {
            return std::ptr::null_mut();
        };
        arr[sz] = TTF_OpenFont(c.as_ptr(), sz as c_int);
        if !arr[sz].is_null() {
            // The same light-hinting contract the base face is opened under (see `font_at`).
            TTF_SetFontHinting(arr[sz], TTF_HINTING_LIGHT);
        }
    }
    arr[sz]
}

/// A string's split into single-face runs. Fixed capacity so the common path allocates nothing;
/// a string that alternates script more than this absorbs its tail into the last run, which
/// degrades to the pre-chain behaviour (tofu for the overflow) instead of failing.
const MAX_RUNS: usize = 24;
struct Runs {
    n: usize,
    at: [(Link, usize, usize); MAX_RUNS], // (face, byte start, byte end)
}

/// Split `s` into maximal runs, each drawable by one face. Returns `None` when the WHOLE string
/// is drawable by [`Link::Base`] — the overwhelmingly common case, and the caller's signal to take
/// the untouched single-render path.
///
/// The ASCII shortcut is the reason an English library pays nothing at all for this feature: it
/// returns before any coverage is loaded, so `appfont-cjk.ttf`'s cmap is never even opened.
unsafe fn split_runs(s: &str) -> Option<Runs> {
    split_runs_with(s, |link, cp| link_covers(link, cp))
}

/// [`split_runs`] with the coverage oracle passed in — the pure half, so the host suite can drive
/// it against the REAL shipped cmaps without a `TTF_Font`, a GL context or an install directory.
/// The `unsafe` half is only the lazy file load behind `link_covers`.
fn split_runs_with(s: &str, covers: impl Fn(Link, u32) -> bool) -> Option<Runs> {
    if s.is_ascii() {
        return None;
    }
    let mut runs = Runs {
        n: 0,
        at: [(Link::Base, 0, 0); MAX_RUNS],
    };
    let mut mixed = false;
    for (off, ch) in s.char_indices() {
        let cp = ch as u32;
        // First covering link wins, ALWAYS — never "stay in the current face because it happens
        // to have this glyph too". Noto Sans CJK carries a full Latin set, so a sticky rule would
        // render "(2024)" in a different typeface from the rest of the interface.
        //
        // A codepoint NO link covers resolves to Base, which draws its .notdef box. That is the
        // right answer: it is the honest tofu, it keeps the runs contiguous, and it is what the
        // renderer did before the chain existed.
        let link = CHAIN
            .iter()
            .copied()
            .find(|&l| covers(l, cp))
            .unwrap_or(Link::Base);
        if link != Link::Base {
            mixed = true;
        }
        let end = off + ch.len_utf8();
        match runs.at.get_mut(..runs.n).and_then(|r| r.last_mut()) {
            Some(last) if last.0 == link => last.2 = end,
            _ if runs.n < MAX_RUNS => {
                runs.at[runs.n] = (link, off, end);
                runs.n += 1;
            }
            // Out of run slots: everything left joins the final run. Extending rather than
            // stopping is what keeps the runs a PARTITION of the string — a gap here would drop
            // characters from the composite silently, which is worse than the wrong face.
            _ => runs.at[MAX_RUNS - 1].2 = s.len(),
        }
    }
    // A single all-Base run is exactly the fast path, however the string is spelled.
    if !mixed {
        return None;
    }
    Some(runs)
}

/// vertical ink bounds of a packed RGBA buffer — the same scan [`surface_ink_v`] does, over a
/// buffer we own rather than an SDL surface.
fn buf_ink_v(px: &[u8], w: c_int, h: c_int) -> (c_int, c_int) {
    let (mut top, mut bot) = (h, -1);
    for y in 0..h {
        let row = &px[(y as usize) * (w as usize) * 4..];
        if (0..w as usize).any(|x| row[x * 4 + 3] > 24) {
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

/// Render each run with its own face and composite them into one packed RGBA buffer.
///
/// Returns `(pixels, w, h)`. Geometry: the composite keeps the BASE face's box — height
/// `TTF_FontHeight`, baseline `TTF_FontAscent` rows down — and each run is placed so its own
/// baseline lands there. The box only ever grows DOWNWARD (a run that descends further than the
/// base face), because growing it upward would move every glyph relative to the caller's `y` and
/// silently mis-align a mixed-script label against its pure-Latin neighbours.
unsafe fn render_runs(
    s: &str,
    runs: &Runs,
    sz: c_int,
    bold: c_int,
) -> Option<(Vec<u8>, c_int, c_int)> {
    let base = font_at(sz, bold);
    if base.is_null() {
        return None;
    }
    let base_asc = TTF_FontAscent(base);
    let white = SdlColor {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    // (surface, x, ascent) per run. Surfaces are freed on every exit path below.
    let mut parts: Vec<(*mut SdlSurface, c_int, c_int)> = Vec::with_capacity(runs.n);
    let free_all = |parts: &Vec<(*mut SdlSurface, c_int, c_int)>| {
        for &(s, _, _) in parts {
            SDL_FreeSurface(s);
        }
    };
    let (mut w, mut below) = (0i32, TTF_FontHeight(base) - base_asc);
    for &(link, from, to) in &runs.at[..runs.n] {
        let f = link_font(link, sz, bold);
        // A link whose face will not open is not fatal: fall back to the base face, which draws
        // the run's .notdef boxes — the pre-chain result for that run only.
        let f = if f.is_null() { base } else { f };
        let Ok(c) = CString::new(&s[from..to]) else {
            free_all(&parts);
            return None;
        };
        let surf = TTF_RenderUTF8_Blended(f, c.as_ptr(), white);
        if surf.is_null() {
            free_all(&parts);
            return None;
        }
        let asc = TTF_FontAscent(f);
        below = below.max((*surf).h - asc);
        parts.push((surf, w, asc));
        w += (*surf).w;
    }
    let h = base_asc + below;
    if w <= 0 || h <= 0 {
        free_all(&parts);
        return None;
    }
    let stride = w as usize * 4;
    let mut out = vec![0u8; stride * h as usize];
    for &(surf, x, asc) in &parts {
        let (sw, sh, pitch) = ((*surf).w, (*surf).h, (*surf).pitch as usize);
        let src = (*surf).pixels as *const u8;
        if src.is_null() {
            continue;
        }
        let dy = base_asc - asc; // negative when the run's face rises higher than the base's
        for row in 0..sh {
            let y = dy + row;
            if y < 0 {
                continue;
            }
            if y >= h {
                break;
            }
            let n = (sw as usize * 4).min(stride - x as usize * 4);
            std::ptr::copy_nonoverlapping(
                src.add(row as usize * pitch),
                out.as_mut_ptr().add(y as usize * stride + x as usize * 4),
                n,
            );
        }
    }
    free_all(&parts);
    Some((out, w, h))
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
        TPROGF = crate::gfx::link_program(VS_TEXT.as_ptr(), FS_TEXT_FADE.as_ptr()).unwrap_or_else(
            || {
                log("text fade prog link failed");
                0
            },
        );
        if TPROGF != 0 {
            TLF_RECT = glGetUniformLocation(TPROGF, c"u_trect".as_ptr());
            TLF_SCREEN = glGetUniformLocation(TPROGF, c"u_tscreen".as_ptr());
            TLF_COL = glGetUniformLocation(TPROGF, c"u_tcol".as_ptr());
            TLF_TEX = glGetUniformLocation(TPROGF, c"u_tex".as_ptr());
            TLF_FADE = glGetUniformLocation(TPROGF, c"u_tfade".as_ptr());
            TLF_VTOP = glGetUniformLocation(TPROGF, c"u_vfadeT".as_ptr());
            TLF_VBOT = glGetUniformLocation(TPROGF, c"u_vfadeB".as_ptr());
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
        // read through a raw pointer, not `&TEXT_OK`: `format!` takes its arguments BY REFERENCE,
        // which is a shared reference to a `static mut` (`static_mut_refs`, a future hard error)
        log(&format!("init_text ok={}", *addr_of!(TEXT_OK)));
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
unsafe fn text_tex(
    s_bytes: &[u8],
    s_c: *const c_char,
    sz: c_int,
    bold: c_int,
) -> (c_uint, c_int, c_int, c_int, c_int) {
    let hash = key_hash(s_bytes, sz, bold);
    {
        let cache = &mut *addr_of_mut!(TCACHE_A);
        for e in cache.iter_mut() {
            // klen + prefix, not full equality: the stored key is truncated to 95 bytes (see
            // TCacheEntry::klen) — starts_with degrades to equality for every short string.
            if e.hash == hash
                && e.tex != 0
                && e.sz == sz
                && e.bold == bold
                && e.klen as usize == s_bytes.len()
                && s_bytes.starts_with(entry_key(e))
            {
                TCLOCK = TCLOCK.wrapping_add(1);
                e.use_ = TCLOCK;
                return (e.tex, e.w, e.h, e.ink_t, e.ink_b);
            }
        }
    }
    // A string the base face cannot draw on its own goes through the run compositor; everything
    // else — every ASCII string, and every string Inter fully covers — takes the single
    // `TTF_RenderUTF8_Blended` it always did, byte for byte. A compositor FAILURE also falls
    // through to that path, which renders the base face's .notdef boxes: the pre-chain result,
    // which is a worse picture but never a missing one.
    let composed = std::str::from_utf8(s_bytes)
        .ok()
        .and_then(|st| Some((st, split_runs(st)?)))
        .and_then(|(st, runs)| render_runs(st, &runs, sz, bold));
    if let Some((px, w, h)) = composed {
        let (ink_t, ink_b) = buf_ink_v(&px, w, h);
        let tex = crate::gfx::upload_rgba(0, w, h, px.as_ptr());
        return cache_store(s_bytes, hash, sz, bold, tex, w, h, ink_t, ink_b);
    }

    let f = font_at(sz, bold);
    if f.is_null() {
        return (0, 0, 0, 0, 0);
    }
    let white = SdlColor {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    let surf = TTF_RenderUTF8_Blended(f, s_c, white);
    if surf.is_null() {
        return (0, 0, 0, 0, 0);
    }
    let (sw, sh, pixels) = ((*surf).w, (*surf).h, (*surf).pixels);
    let (ink_t, ink_b) = surface_ink_v(surf);
    // Honour the surface's PITCH. SDL is free to pad each row, and `pitch` — not `w * 4` — is the
    // stride it actually wrote; `surface_ink_v` above has always read it, and only this upload
    // assumed the two were equal. They are on the television's SDL2_ttf, which is why this was
    // invisible for the app's whole life; they are NOT on the desktop SDL2_ttf the simulator
    // links, where every string rendered as diagonal hatching because each row was read four bytes
    // early and the image sheared.
    //
    // Repacking rather than `GL_UNPACK_ROW_LENGTH`: that pixel-store parameter does not exist in
    // GLES2, so the one-line fix would work on the simulator and silently do nothing on the TV.
    // The copy runs only when the stride is actually padded, so the device path is unchanged.
    let stride = (*surf).pitch as isize;
    let tight = sw as isize * 4;
    let tex = if stride == tight {
        crate::gfx::upload_rgba(0, sw, sh, pixels as *const u8)
    } else {
        let mut packed = vec![0u8; (tight * sh as isize) as usize];
        let base = pixels as *const u8;
        for y in 0..sh as isize {
            std::ptr::copy_nonoverlapping(
                base.offset(y * stride),
                packed.as_mut_ptr().offset(y * tight),
                tight as usize,
            );
        }
        crate::gfx::upload_rgba(0, sw, sh, packed.as_ptr())
    };
    SDL_FreeSurface(surf);
    cache_store(s_bytes, hash, sz, bold, tex, sw, sh, ink_t, ink_b)
}

/// Evict the LRU slot and install a freshly uploaded texture in it, returning what `text_tex`
/// returns. Split out because there are now TWO ways to get a texture — one
/// `TTF_RenderUTF8_Blended`, or a composite of several — and exactly one cache to put it in.
#[allow(clippy::too_many_arguments)]
unsafe fn cache_store(
    s_bytes: &[u8],
    hash: u64,
    sz: c_int,
    bold: c_int,
    tex: c_uint,
    sw: c_int,
    sh: c_int,
    ink_t: c_int,
    ink_b: c_int,
) -> (c_uint, c_int, c_int, c_int, c_int) {
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
            log(&format!(
                "text cache: key len {} exceeds the 95-byte prefix (fine, klen-keyed)",
                s_bytes.len()
            ));
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
///
/// **A mixed-script string is measured per RUN and summed**, because that is how it is drawn. The
/// two agree exactly rather than approximately: SDL_ttf sizes a blended surface with this very
/// call, so each run's `TTF_SizeUTF8` width IS the width `render_runs` advances the cursor by.
/// (Cross-run kerning is lost — there is no such thing between a Latin and a Han glyph, and the
/// faces are different files, so no kern table spans the boundary.)
///
/// The cost model changes for those strings only, and both callers that hammer this absorb it:
/// `elide`'s binary search is memoised on its RESULT, and `TextView`'s wrap sweep goes through
/// `elide`. A pure-ASCII string still costs exactly one `TTF_SizeUTF8`, decided before any
/// coverage is loaded.
pub(crate) fn text_width(s: *const c_char, sz: c_int, bold: c_int) -> f32 {
    unsafe {
        if TEXT_OK == 0 || s.is_null() || *s == 0 {
            return 0.0;
        }
        let bytes = CStr::from_ptr(s).to_bytes();
        if let Some((st, runs)) = std::str::from_utf8(bytes)
            .ok()
            .and_then(|st| Some((st, split_runs(st)?)))
        {
            let mut total = 0.0f32;
            for &(link, from, to) in &runs.at[..runs.n] {
                let f = link_font(link, sz, bold);
                let f = if f.is_null() { font_at(sz, bold) } else { f };
                let Ok(c) = CString::new(&st[from..to]) else {
                    continue;
                };
                let (mut w, mut h): (c_int, c_int) = (0, 0);
                if !f.is_null() && TTF_SizeUTF8(f, c.as_ptr(), &mut w, &mut h) == 0 {
                    total += w as f32;
                }
            }
            return total;
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
    let measure = |t: &str| {
        CString::new(t)
            .ok()
            .map(|c| text_width(c.as_ptr(), sz, bold))
            .unwrap_or(0.0)
    };
    let target = if cont {
        format!("{s}\u{2026}")
    } else {
        s.to_string()
    };
    if budget <= 0.0 || measure(&target) <= budget {
        return target;
    }
    // largest char-prefix of `s` whose "prefix…" still fits `budget`
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cand = chars[..mid]
            .iter()
            .collect::<String>()
            .trim_end()
            .to_string()
            + "\u{2026}";
        if measure(&cand) <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo]
        .iter()
        .collect::<String>()
        .trim_end()
        .to_string()
        + "\u{2026}"
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

/// The draw-`y` at which text of `sz`/`bold` sits on the **baseline** of a run of `on_sz`/`on_bold`
/// text drawn at `on_y` — the one-line twin of [`text_vcenter_y`], for two runs of DIFFERENT sizes
/// on the same line.
///
/// Two such runs must align by baseline, never by top: a caption beside a number, a `·` and a
/// handle beside a heading. Top-aligning them hangs the smaller run's whole cap band above the
/// larger one's baseline, so it reads as a superscript; the design mocks say `align-items:baseline`
/// for exactly this. The subtraction is the cap-band bottoms, which is a font metric and therefore
/// string-independent (see [`text_cap_band`]).
///
/// The ONE place this arithmetic lives — `widgets::rating_group` had written it out inline, and the
/// shelf heading's source annotation needs the same three lines.
///
/// The two band bottoms are subtracted from EACH OTHER before `on_y` is touched, deliberately: a
/// run measured against its own tokens then returns `on_y` bit-for-bit, so a caller that resolves
/// every run of a flow through this (`ui::home::draw_heading`) does not nudge its reference run by
/// an ULP for the privilege.
pub(crate) fn baseline_y(sz: c_int, bold: c_int, on_sz: c_int, on_bold: c_int, on_y: f32) -> f32 {
    // The same-token case is the COMMON one — a flow resolves its own reference run through here
    // too — and it is not free: `text_cap_band` rasterizes through the glyph cache, whose lookup is
    // a linear scan of 160 entries. Twice per heading, per shelf, per frame, inside the grid draw
    // loop, to compute a difference that is zero by construction. Take it as an identity instead.
    if sz == on_sz && bold == on_bold {
        return on_y;
    }
    on_y + (text_cap_band(on_sz, on_bold).1 - text_cap_band(sz, bold).1)
}

/// align: 0 left, 1 center, 2 right (x is the anchor edge). returns text width.
pub(crate) fn draw_text(
    s: *const c_char,
    x: f32,
    y: f32,
    sz: c_int,
    col: *const f32,
    align: c_int,
    bold: c_int,
) -> f32 {
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
        glUniform4f(
            TL_RECT,
            crate::gfx::snap(dx),
            crate::gfx::snap(y),
            w as f32,
            h as f32,
        );
        // The width is still returned — callers lay out from it — but a run outside a blur source
        // pass's region contributes no fragment to the backdrop, so the quad is not submitted.
        if !crate::gfx::culled(dx, y, w as f32, h as f32)
            && !gate(Class::Text, dx, y, w as f32, h as f32)
        {
            glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        }
        w as f32
    }
}

/// [`draw_text`] with up to three independent fades (its OWN GL program, so plain text pays no
/// per-fragment fade cost — see the shader's own header for why every ordinary glyph stays off
/// it): a HORIZONTAL one (`hfade`, glyph alpha 1→0 between `from`..`to` px from the string's LEFT
/// edge regardless of `align` — the About card's and the person header bio's `MORE` dissolve), and
/// two VERTICAL ones in absolute logical screen y (`vfade_top` ramps 0→1 rising through the band,
/// `vfade_bot` ramps 1→0 falling through it — a line crossing a SCROLLING viewport's clipped edge,
/// see `ui::text_view::TextView::edge_fade`). Each is `None` by default and costs the shader
/// nothing extra to skip (a uniform compare, not a texture sample). Falls back to plain
/// [`draw_text`] if the fade program failed to link — a device with no working fade shader still
/// shows every word, just without the dissolve.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_text_fade(
    s: *const c_char,
    x: f32,
    y: f32,
    sz: c_int,
    col: *const f32,
    align: c_int,
    bold: c_int,
    hfade: Option<(f32, f32)>,
    vfade_top: Option<(f32, f32)>,
    vfade_bot: Option<(f32, f32)>,
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
        // px → string-texture uv (the varying spans the one-quad string). `(0.0, 0.0)` is "off" —
        // the shader gates on `to > from`, so a caller with no horizontal fade need not sentinel
        // against the string's own width.
        let wf = w as f32;
        let (hf0, hf1) = hfade.unwrap_or((0.0, 0.0));
        glUniform2f(TLF_FADE, hf0 / wf, hf1 / wf);
        let (vt0, vt1) = vfade_top.unwrap_or((0.0, 0.0));
        glUniform2f(TLF_VTOP, vt0, vt1);
        let (vb0, vb1) = vfade_bot.unwrap_or((0.0, 0.0));
        glUniform2f(TLF_VBOT, vb0, vb1);
        glBindTexture(GL_TEXTURE_2D, tex);
        glUniform4f(
            TLF_RECT,
            crate::gfx::snap(dx),
            crate::gfx::snap(y),
            w as f32,
            h as f32,
        );
        if !crate::gfx::culled(dx, y, w as f32, h as f32)
            && !gate(Class::Text, dx, y, w as f32, h as f32)
        {
            glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        }
        w as f32
    }
}

#[cfg(test)]
mod tests {
    //! The RUN SPLITTER, against the real shipped cmaps.
    //!
    //! Everything else in this module needs a `TTF_Font`, a GL context and a television, which is
    //! exactly why the chain's *decision* was factored out of its *rendering*: `split_runs_with`
    //! is pure, and the coverage it consults is a property of files this repository ships. So the
    //! half that decides which face draws which characters is gradable in `make check`, and only
    //! the rasterization is left to the device.
    //!
    //! What these CANNOT see, and the reason a device capture is still required: whether the
    //! composite is aligned, hinted and snapped correctly on the panel. `cargo test` runs on
    //! Darwin against a different FreeType, and the simulator renders through desktop GL — the ♪
    //! regression that named this unit was found by a photograph, not by a test.

    use super::*;
    use crate::fontcov;

    /// The real shipped cmaps, parsed once for the whole test binary (see `fontcov::shipped`).
    /// `Link::Sys` is `None` on purpose: the television's DroidSansFallback does not exist on a
    /// build host, so this is also the "the chain is SHORT" configuration under test.
    fn chain() -> [Option<&'static fontcov::Coverage>; 3] {
        [
            fontcov::shipped("appfont.ttf").as_ref().ok(),
            fontcov::shipped("appfont-cjk.ttf").as_ref().ok(),
            None,
        ]
    }

    fn split(s: &str) -> Option<Runs> {
        let cov = chain();
        split_runs_with(s, |link, cp| {
            cov[link as usize].is_some_and(|c| c.contains(cp))
        })
    }

    /// Every run, as (face, the text it draws).
    fn runs_of(s: &str) -> Vec<(Link, &str)> {
        match split(s) {
            None => vec![(Link::Base, s)],
            Some(r) => r.at[..r.n].iter().map(|&(l, a, b)| (l, &s[a..b])).collect(),
        }
    }

    /// The one invariant the compositor's correctness rests on: the runs PARTITION the string —
    /// contiguous, in order, covering every byte exactly once. A gap drops characters from the
    /// picture with nothing in any log; an overlap draws them twice.
    fn assert_partitions(s: &str) {
        let Some(r) = split(s) else { return };
        let mut at = 0usize;
        for &(_, from, to) in &r.at[..r.n] {
            assert_eq!(
                from, at,
                "run starts at {from}, previous ended at {at}, in {s:?}"
            );
            assert!(to > from, "empty run in {s:?}");
            at = to;
        }
        assert_eq!(
            at,
            s.len(),
            "runs stop at byte {at} of {} in {s:?}",
            s.len()
        );
        let joined: String = r.at[..r.n].iter().map(|&(_, a, b)| &s[a..b]).collect();
        assert_eq!(joined, s, "the runs do not reassemble the string");
    }

    /// Anything Inter fully covers must return `None`, which is the caller's signal to take the
    /// single-render path byte for byte. This is the performance contract of the whole feature:
    /// an English or Russian library pays nothing, and never even opens the 21 MB fallback.
    #[test]
    fn strings_the_base_face_covers_take_the_untouched_fast_path() {
        for s in [
            "Breaking Bad",                     // pure ASCII — decided before any cmap is read
            "Amélie",                           // Latin-1
            "Ирония судьбы",                    // Cyrillic
            "Ο Θίασος",                         // Greek
            "\u{266A} It seems today \u{266B}", // the ♪ regression: Inter carries U+2669..U+266C
            "S1 · E1 — 47 min",                 // the punctuation the UI composes by hand
        ] {
            assert!(
                split(s).is_none(),
                "{s:?} should not need the fallback chain"
            );
        }
    }

    #[test]
    fn a_korean_title_splits_onto_the_bundled_face() {
        assert_eq!(
            runs_of("오징어 게임 (2021)"),
            vec![
                (Link::Cjk, "오징어"),
                (Link::Base, " "),
                (Link::Cjk, "게임"),
                (Link::Base, " (2021)")
            ],
        );
    }

    /// Japanese mixes three scripts inside one word boundary, and Chinese shares Han with Korean.
    /// Both must land on the same single fallback face rather than fragmenting further.
    #[test]
    fn japanese_and_chinese_land_on_one_face() {
        assert_eq!(runs_of("君の名は。"), vec![(Link::Cjk, "君の名は。")]);
        assert_eq!(runs_of("ドラえもん"), vec![(Link::Cjk, "ドラえもん")]);
        assert_eq!(runs_of("臥虎藏龍"), vec![(Link::Cjk, "臥虎藏龍")]);
        assert_eq!(
            runs_of("流浪地球 The Wandering Earth"),
            vec![
                (Link::Cjk, "流浪地球"),
                (Link::Base, " The Wandering Earth")
            ],
        );
    }

    /// Noto Sans CJK carries a complete Latin set. If the splitter ever went "sticky" — staying in
    /// the current face while it happens to cover the next character — a title like this would
    /// render its Latin half in a DIFFERENT TYPEFACE from the rest of the interface, which is a
    /// subtle enough wrong to survive review.
    #[test]
    fn latin_after_a_cjk_run_returns_to_the_app_typeface() {
        assert_eq!(
            runs_of("東京物語 1953"),
            vec![(Link::Cjk, "東京物語"), (Link::Base, " 1953")]
        );
        assert_eq!(
            runs_of("A한B"),
            vec![(Link::Base, "A"), (Link::Cjk, "한"), (Link::Base, "B")]
        );
    }

    /// A codepoint no link covers stays on the base face and stays INSIDE the run structure, so it
    /// draws a box where it belongs rather than vanishing. Hebrew is the honest example, because
    /// the chain deliberately has no face for it (see the module header — coverage is not support).
    #[test]
    fn an_uncoverable_codepoint_is_a_box_not_a_gap() {
        assert_eq!(
            runs_of("한글 \u{05D0}\u{05D1}"),
            vec![(Link::Cjk, "한글"), (Link::Base, " \u{05D0}\u{05D1}")]
        );
        assert_partitions("한글 \u{05D0}\u{05D1}");
    }

    /// The run array is fixed-capacity, and the overflow policy has to keep the partition. A
    /// string that alternates on every character is the worst case by construction.
    #[test]
    fn overflowing_the_run_array_still_partitions_the_string() {
        let pathological: String = (0..MAX_RUNS * 3)
            .map(|i| if i % 2 == 0 { 'A' } else { '한' })
            .collect();
        assert_partitions(&pathological);
        let r = split(&pathological).expect("a mixed string splits");
        assert_eq!(r.n, MAX_RUNS, "the array is full");
        assert_eq!(
            r.at[MAX_RUNS - 1].2,
            pathological.len(),
            "the tail was absorbed, not dropped"
        );
    }

    /// Byte-indexed boundary arithmetic on multi-byte characters at the very edges of the string,
    /// where an off-by-one would slice mid-codepoint and panic inside `&s[a..b]`.
    #[test]
    fn multibyte_characters_at_both_edges_are_sliced_on_char_boundaries() {
        for s in [
            "한",
            "한A",
            "A한",
            "한A한",
            "\u{1F600}한",
            "君の名は。 2016",
            "ラーメン大好き小泉さん",
        ] {
            assert_partitions(s);
        }
    }
}
