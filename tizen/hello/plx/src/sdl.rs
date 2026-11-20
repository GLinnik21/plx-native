//! Hand-written bindings to the STATIC SDL2 and SDL2_ttf archives (`sdl/build-sdl.sh`), linked into
//! this library by `build.rs`. Compiled only with `--cfg plx_sdl`.
//!
//! Only what the H5 probe calls is declared. The few structs and constants spelled out here are
//! compared against the installed headers, on every arch, by `sdl/layout-check.c` (compile-only, run by
//! `build-sdl.sh`): change the two together. Nothing here opens an SDL window: the video SUBSYSTEM is
//! never initialised, only the surface/blit half of SDL (which needs no init) is used.
//!
//! The unsafe surface is kept to this file: `Surface` and `Font` own their C objects (freed on drop) and
//! every fallible call returns the text of `SDL_GetError()`.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr::NonNull;

pub const INIT_TIMER: u32 = 0x0000_0001;
pub const INIT_EVENTS: u32 = 0x0000_4000;
pub const PIXELFORMAT_ARGB8888: u32 = 0x1636_2004;
pub const PIXELFORMAT_ABGR8888: u32 = 0x1676_2004;
pub const BLENDMODE_BLEND: c_int = 1;
/// SDL_ttf's and SDL's "no signal handlers" hint (`SDL_HINT_NO_SIGNAL_HANDLERS`).
pub const HINT_NO_SIGNAL_HANDLERS: &CStr = c"SDL_NO_SIGNAL_HANDLERS";

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub x: c_int,
    pub y: c_int,
    pub w: c_int,
    pub h: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Only the first field of `SDL_PixelFormat` is read.
#[repr(C)]
pub struct PixelFormat {
    pub format: u32,
}

/// The layout is what matters (sdl/layout-check.c); not every field is read.
#[repr(C)]
#[allow(dead_code)]
pub struct RawSurface {
    flags: u32,
    pub format: *mut PixelFormat,
    pub w: c_int,
    pub h: c_int,
    pub pitch: c_int,
    pub pixels: *mut c_void,
    userdata: *mut c_void,
    locked: c_int,
    list_blitmap: *mut c_void,
    clip_rect: Rect,
    map: *mut c_void,
    refcount: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Version {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

type TimerCb = unsafe extern "C" fn(interval: u32, param: *mut c_void) -> u32;

extern "C" {
    // --- SDL2 core ---
    fn SDL_SetHint(name: *const c_char, value: *const c_char) -> c_int;
    fn SDL_GetHint(name: *const c_char) -> *const c_char;
    fn SDL_Init(flags: u32) -> c_int;
    fn SDL_WasInit(flags: u32) -> u32;
    fn SDL_Quit();
    fn SDL_GetError() -> *const c_char;
    fn SDL_GetVersion(ver: *mut Version);
    fn SDL_GetRevision() -> *const c_char;
    fn SDL_GetTicks64() -> u64;
    fn SDL_Delay(ms: u32);
    fn SDL_AddTimer(interval: u32, cb: TimerCb, param: *mut c_void) -> c_int;
    fn SDL_RemoveTimer(id: c_int) -> c_int;
    fn SDL_PumpEvents();
    fn SDL_PollEvent(event: *mut c_void) -> c_int;
    // --- surfaces ---
    fn SDL_CreateRGBSurfaceWithFormat(flags: u32, w: c_int, h: c_int, depth: c_int, format: u32) -> *mut RawSurface;
    fn SDL_FreeSurface(s: *mut RawSurface);
    fn SDL_ConvertSurfaceFormat(s: *mut RawSurface, format: u32, flags: u32) -> *mut RawSurface;
    fn SDL_FillRect(dst: *mut RawSurface, rect: *const Rect, color: u32) -> c_int;
    fn SDL_MapRGBA(fmt: *const PixelFormat, r: u8, g: u8, b: u8, a: u8) -> u32;
    fn SDL_SetSurfaceBlendMode(s: *mut RawSurface, mode: c_int) -> c_int;
    /// `SDL_BlitSurface` is a macro for this.
    fn SDL_UpperBlit(src: *mut RawSurface, srcrect: *const Rect, dst: *mut RawSurface, dstrect: *mut Rect) -> c_int;
    fn SDL_GetPixelFormatName(format: u32) -> *const c_char;
    // --- SDL2_ttf ---
    fn TTF_Init() -> c_int;
    fn TTF_Quit();
    fn TTF_WasInit() -> c_int;
    fn TTF_Linked_Version() -> *const Version;
    fn TTF_GetFreeTypeVersion(major: *mut c_int, minor: *mut c_int, patch: *mut c_int);
    fn TTF_GetHarfBuzzVersion(major: *mut c_int, minor: *mut c_int, patch: *mut c_int);
    fn TTF_OpenFont(file: *const c_char, ptsize: c_int) -> *mut c_void;
    fn TTF_CloseFont(font: *mut c_void);
    fn TTF_FontHeight(font: *const c_void) -> c_int;
    fn TTF_SetFontLineSkip(font: *mut c_void, lineskip: c_int);
    fn TTF_FontFaceFamilyName(font: *const c_void) -> *const c_char;
    fn TTF_FontFaceStyleName(font: *const c_void) -> *const c_char;
    fn TTF_GlyphIsProvided32(font: *mut c_void, ch: u32) -> c_int;
    fn TTF_SizeUTF8(font: *mut c_void, text: *const c_char, w: *mut c_int, h: *mut c_int) -> c_int;
    fn TTF_RenderUTF8_Blended(font: *mut c_void, text: *const c_char, fg: Color) -> *mut RawSurface;
    fn TTF_RenderUTF8_Blended_Wrapped(font: *mut c_void, text: *const c_char, fg: Color, wrap_length: u32) -> *mut RawSurface;
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        // SAFETY: SDL returns NUL-terminated strings that stay valid until the next SDL call that
        // sets them; copied at once.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// `SDL_GetError()`, or a marker when SDL left it empty (SDL does not always set one).
pub fn last_error() -> String {
    let e = cstr(unsafe { SDL_GetError() });
    if e.is_empty() {
        "(SDL_GetError is empty)".to_string()
    } else {
        e
    }
}

/// SDL's runtime version and source revision string.
pub fn version() -> (Version, String) {
    let mut v = Version::default();
    unsafe { SDL_GetVersion(&mut v) };
    (v, cstr(unsafe { SDL_GetRevision() }))
}

pub fn set_hint(name: &CStr, value: &CStr) -> bool {
    unsafe { SDL_SetHint(name.as_ptr(), value.as_ptr()) != 0 }
}

pub fn get_hint(name: &CStr) -> Option<String> {
    let p = unsafe { SDL_GetHint(name.as_ptr()) };
    (!p.is_null()).then(|| cstr(p))
}

/// `SDL_Init(flags)`: `Ok(())` or the SDL error text.
pub fn init(flags: u32) -> Result<(), String> {
    if unsafe { SDL_Init(flags) } == 0 {
        Ok(())
    } else {
        Err(last_error())
    }
}

pub fn was_init(flags: u32) -> u32 {
    unsafe { SDL_WasInit(flags) }
}

pub fn quit() {
    unsafe { SDL_Quit() }
}

pub fn ticks_ms() -> u64 {
    unsafe { SDL_GetTicks64() }
}

pub fn delay_ms(ms: u32) {
    unsafe { SDL_Delay(ms) }
}

/// A one-shot timer: calls `cb(param)` once on SDL's timer thread after `ms`. Returns the timer id.
pub fn add_timer(ms: u32, cb: TimerCb, param: *mut c_void) -> Result<c_int, String> {
    let id = unsafe { SDL_AddTimer(ms, cb, param) };
    if id == 0 {
        Err(last_error())
    } else {
        Ok(id)
    }
}

pub fn remove_timer(id: c_int) -> bool {
    unsafe { SDL_RemoveTimer(id) != 0 }
}

/// Pump the event queue and drain it. Returns how many events were waiting (0 is normal: no video, no
/// input subsystems were initialised).
pub fn pump_and_drain_events() -> u32 {
    // SDL_Event is a 56-byte union; 64 aligned bytes (checked by sdl/layout-check.c).
    let mut buf = [0u64; 8];
    unsafe { SDL_PumpEvents() };
    let mut n = 0;
    while unsafe { SDL_PollEvent(buf.as_mut_ptr().cast()) } != 0 && n < 1000 {
        n += 1;
    }
    n
}

pub fn pixel_format_name(format: u32) -> String {
    cstr(unsafe { SDL_GetPixelFormatName(format) })
}

/// An owned `SDL_Surface`, freed on drop.
pub struct Surface(NonNull<RawSurface>);

impl Surface {
    fn wrap(p: *mut RawSurface) -> Result<Surface, String> {
        NonNull::new(p).map(Surface).ok_or_else(last_error)
    }

    pub fn new_rgb(w: i32, h: i32, format: u32) -> Result<Surface, String> {
        // SAFETY: plain allocation; a null return is reported through SDL_GetError.
        Surface::wrap(unsafe { SDL_CreateRGBSurfaceWithFormat(0, w, h, 32, format) })
    }

    fn raw(&self) -> &RawSurface {
        // SAFETY: the pointer is non-null and owned by us until drop.
        unsafe { self.0.as_ref() }
    }

    pub fn width(&self) -> i32 {
        self.raw().w
    }

    pub fn height(&self) -> i32 {
        self.raw().h
    }

    pub fn pitch(&self) -> i32 {
        self.raw().pitch
    }

    pub fn format(&self) -> u32 {
        let f = self.raw().format;
        if f.is_null() {
            0
        } else {
            // SAFETY: a live surface's `format` points at its SDL_PixelFormat.
            unsafe { (*f).format }
        }
    }

    /// The pixel rows, `pitch * height` bytes (rows may be padded beyond `width * 4`).
    pub fn bytes(&self) -> &[u8] {
        let r = self.raw();
        if r.pixels.is_null() || r.pitch <= 0 || r.h <= 0 {
            return &[];
        }
        // SAFETY: SDL allocates `pitch * h` bytes for a surface it created; the borrow ends before
        // the surface can be dropped.
        unsafe { std::slice::from_raw_parts(r.pixels.cast::<u8>(), r.pitch as usize * r.h as usize) }
    }

    pub fn fill(&mut self, r: u8, g: u8, b: u8, a: u8) -> Result<(), String> {
        let raw = self.0.as_ptr();
        // SAFETY: valid surface; a null rect fills all of it.
        let color = unsafe { SDL_MapRGBA((*raw).format, r, g, b, a) };
        if unsafe { SDL_FillRect(raw, std::ptr::null(), color) } == 0 {
            Ok(())
        } else {
            Err(last_error())
        }
    }

    pub fn set_blend(&mut self, mode: c_int) -> Result<(), String> {
        if unsafe { SDL_SetSurfaceBlendMode(self.0.as_ptr(), mode) } == 0 {
            Ok(())
        } else {
            Err(last_error())
        }
    }

    /// Blit all of `self` onto `dst` with its top-left at `(x, y)`.
    pub fn blit_onto(&mut self, dst: &mut Surface, x: i32, y: i32) -> Result<(), String> {
        let mut at = Rect { x, y, w: 0, h: 0 };
        if unsafe { SDL_UpperBlit(self.0.as_ptr(), std::ptr::null(), dst.0.as_ptr(), &mut at) } == 0 {
            Ok(())
        } else {
            Err(last_error())
        }
    }

    pub fn convert(&self, format: u32) -> Result<Surface, String> {
        Surface::wrap(unsafe { SDL_ConvertSurfaceFormat(self.0.as_ptr(), format, 0) })
    }

    /// The surface as tightly packed, fully opaque RGBA bytes (`width * 4` per row, top row first).
    /// For an opaque plate: SDL's software BLEND blit leaves alpha at 254 on anti-aliased pixels, and
    /// the GL stage draws without blending, so alpha is made exact instead of leaving a 1/255 hole.
    pub fn to_rgba_opaque(&self) -> Result<Vec<u8>, String> {
        let rgba = self.convert(PIXELFORMAT_ABGR8888)?;
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        let (pitch, row) = (rgba.pitch() as usize, w * 4);
        let bytes = rgba.bytes();
        if pitch < row || bytes.len() < pitch * h {
            return Err(format!("converted surface is smaller than expected (pitch {pitch}, {} bytes)", bytes.len()));
        }
        let mut out = Vec::with_capacity(row * h);
        for y in 0..h {
            out.extend_from_slice(&bytes[y * pitch..y * pitch + row]);
        }
        for p in out.chunks_exact_mut(4) {
            p[3] = 255;
        }
        Ok(out)
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: owned, not freed elsewhere.
        unsafe { SDL_FreeSurface(self.0.as_ptr()) }
    }
}

pub fn ttf_init() -> Result<(), String> {
    if unsafe { TTF_Init() } == 0 {
        Ok(())
    } else {
        Err(last_error())
    }
}

pub fn ttf_quit() {
    unsafe { TTF_Quit() }
}

pub fn ttf_was_init() -> bool {
    unsafe { TTF_WasInit() != 0 }
}

pub fn ttf_linked_version() -> Version {
    let p = unsafe { TTF_Linked_Version() };
    if p.is_null() {
        Version::default()
    } else {
        // SAFETY: static const struct inside the library.
        unsafe { *p }
    }
}

/// `(FreeType, HarfBuzz)` versions SDL_ttf was built with (HarfBuzz is 0.0.0 when it is off).
pub fn ttf_backend_versions() -> ((i32, i32, i32), (i32, i32, i32)) {
    let (mut a, mut b, mut c) = (0, 0, 0);
    unsafe { TTF_GetFreeTypeVersion(&mut a, &mut b, &mut c) };
    let ft = (a, b, c);
    let (mut a, mut b, mut c) = (0, 0, 0);
    unsafe { TTF_GetHarfBuzzVersion(&mut a, &mut b, &mut c) };
    (ft, (a, b, c))
}

/// An owned `TTF_Font`, closed on drop.
pub struct Font(NonNull<c_void>);

impl Font {
    pub fn open(path: &str, px: i32) -> Result<Font, String> {
        let c = CString::new(path).map_err(|_| "font path has a NUL".to_string())?;
        // SAFETY: valid C string; null on failure with SDL_GetError set.
        NonNull::new(unsafe { TTF_OpenFont(c.as_ptr(), px) }).map(Font).ok_or_else(last_error)
    }

    pub fn height(&self) -> i32 {
        unsafe { TTF_FontHeight(self.0.as_ptr()) }
    }

    /// Set the distance between two wrapped lines, in pixels. The glyphs keep their size; only the pitch of
    /// the lines moves (`TTF_SetFontLineSkip`, SDL2_ttf 2.22 and later; the pinned build is 2.24.0).
    pub fn set_line_skip(&mut self, px: i32) {
        unsafe { TTF_SetFontLineSkip(self.0.as_ptr(), px) }
    }

    pub fn family(&self) -> String {
        cstr(unsafe { TTF_FontFaceFamilyName(self.0.as_ptr()) })
    }

    pub fn style(&self) -> String {
        cstr(unsafe { TTF_FontFaceStyleName(self.0.as_ptr()) })
    }

    pub fn has_glyph(&self, ch: char) -> bool {
        unsafe { TTF_GlyphIsProvided32(self.0.as_ptr(), ch as u32) != 0 }
    }

    pub fn size_utf8(&self, text: &str) -> Result<(i32, i32), String> {
        let c = CString::new(text).map_err(|_| "text has a NUL".to_string())?;
        let (mut w, mut h) = (0, 0);
        if unsafe { TTF_SizeUTF8(self.0.as_ptr(), c.as_ptr(), &mut w, &mut h) } == 0 {
            Ok((w, h))
        } else {
            Err(last_error())
        }
    }

    /// Anti-aliased text with per-pixel alpha, in `fg`. An ARGB8888 surface.
    pub fn render_blended(&self, text: &str, fg: Color) -> Result<Surface, String> {
        let c = CString::new(text).map_err(|_| "text has a NUL".to_string())?;
        // SAFETY: valid font and C string; `Color` is passed by value as the C ABI says.
        Surface::wrap(unsafe { TTF_RenderUTF8_Blended(self.0.as_ptr(), c.as_ptr(), fg) })
    }

    /// Like [`Font::render_blended`] for text with `\n` and long lines: breaks at `\n`, and at spaces
    /// (or inside a word that is longer than the line) so no line is wider than `wrap_px`. The text
    /// must not be empty (SDL_ttf refuses a zero-width render).
    pub fn render_blended_wrapped(&self, text: &str, fg: Color, wrap_px: u32) -> Result<Surface, String> {
        let c = CString::new(text).map_err(|_| "text has a NUL".to_string())?;
        // SAFETY: valid font and C string; `Color` is passed by value as the C ABI says.
        Surface::wrap(unsafe { TTF_RenderUTF8_Blended_Wrapped(self.0.as_ptr(), c.as_ptr(), fg, wrap_px) })
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: owned, not closed elsewhere.
        unsafe { TTF_CloseFont(self.0.as_ptr()) }
    }
}
