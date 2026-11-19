//! Probe H5: can the .NET-hosted `libplx.so` carry a STATICALLY linked SDL2 + SDL2_ttf and use them
//! for text, while the window and EGL stay ours (Ecore_Wl2, as in step 1)?
//!
//! Two halves, in two threads, each logging every step (an `H5 ...` line, with `SDL_GetError()` on
//! failure) through the normal log (file + UDP):
//!
//! 1. [`run`], on the host's main thread, at the START of `plx_tizen_create` (before any Ecore or EGL
//!    work, so a device that cannot open a window still answers the SDL and font questions):
//!    `SDL_Init(TIMER | EVENTS)` (the video subsystem is NOT initialised and SDL opens no window), a
//!    timer and event-queue smoke test, `TTF_Init`, open the bundled font (`<res dir>/appfont.ttf`,
//!    the app's own Inter, SIL OFL), render [`TEXT`] to an `SDL_Surface`, set it on a dark plate,
//!    convert it to RGBA bytes. Only plain bytes ([`Label`]) leave this half.
//! 2. `label_gl`, on the render thread: upload the bytes as a GLES2 texture and draw it centred on
//!    every frame, over the step-1 clear colour. The first presented frame with the label ends the
//!    probe with the `H5 RESULT:` line.
//!
//! Every H5 failure is non-fatal (it never becomes a `plx_tizen_create` code): step 1 (window, colour
//! change on keys) must keep working whatever SDL does. Exactly one `H5 RESULT:` line is written.
//!
//! The SDL calls live in `sdl.rs` and exist only when `build.rs` linked the archives (`--cfg
//! plx_sdl`); a library built without them logs that and skips the probe.

use crate::log::plog;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// What the label says: Latin, a symbol the font must have (U+2713) and Cyrillic.
#[cfg(plx_sdl)]
pub const TEXT: &str = "PlxNative H5 \u{2713} \u{041F}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442}";
/// Bundled by `build.sh` into the package's `res/` (a copy of the repository's `pkg/appfont.ttf`).
#[cfg(plx_sdl)]
pub const FONT_FILE: &str = "appfont.ttf";
/// The font's size in pixels (SDL_ttf's point size is the pixel em at 72 dpi).
#[cfg(plx_sdl)]
pub const FONT_PX: i32 = 72;
/// Plate padding around the text, in pixels.
#[cfg(plx_sdl)]
pub const PAD: i32 = 28;

/// The finished label: tightly packed RGBA bytes (`width * 4` per row, top row first, straight
/// alpha, fully opaque because of the plate). Owned plain data: no SDL object crosses a thread.
pub struct Label {
    pub rgba: Vec<u8>,
    pub width: i32,
    pub height: i32,
}

static FIRST_FAILURE: Mutex<Option<String>> = Mutex::new(None);
static RESULT_WRITTEN: AtomicBool = AtomicBool::new(false);

/// Record a failed step (logged now; the first one names the failure in the final line).
pub fn fail(step: &str, why: &str) {
    plog!("H5 FAILED at {step}: {why}");
    let mut first = FIRST_FAILURE.lock().unwrap_or_else(|e| e.into_inner());
    if first.is_none() {
        *first = Some(format!("{step}: {why}"));
    }
}

fn result_once(line: &str) {
    if !RESULT_WRITTEN.swap(true, Ordering::AcqRel) {
        plog!("H5 RESULT: {line}");
    }
}

/// The GL half reports here after the first frame with the label (or its failure).
pub fn gl_done(outcome: Result<(), String>) {
    match outcome {
        Ok(()) => {
            let first = FIRST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()).clone();
            match first {
                None => result_once(
                    "OK (static SDL2 + SDL2_ttf: SDL_Init, TTF_Init, font open, UTF-8 render to SDL_Surface, \
                     GLES2 texture upload and draw on the render thread; the label is on screen)",
                ),
                Some(f) => result_once(&format!("FAILED, first failure at {f} (the label was still drawn)")),
            }
        }
        Err(why) => {
            fail("gl", &why);
            result_once(&format!("FAILED at gl: {why}"));
        }
    }
}

/// `plx_tizen_create` ended before the render thread could report (or the probe never produced a
/// label): close the probe with what is known.
pub fn incomplete(reason: &str) {
    let first = FIRST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match first {
        Some(f) => result_once(&format!("FAILED at {f}")),
        None => result_once(&format!("INCOMPLETE: the SDL + TTF stage passed, the GL stage was not reached ({reason})")),
    }
}

/// Stage 1 on the main thread. `res_dir` is the package's `res/` directory (the host's
/// `DirectoryInfo.Resource`). Returns the label when every SDL and TTF step passed.
pub fn run(res_dir: Option<&str>) -> Option<Label> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| imp::probe(res_dir))) {
        Ok(label) => label,
        Err(_) => {
            fail("panic", "the probe panicked (swallowed; step 1 carries on)");
            None
        }
    }
}

/// `TTF_Quit` + `SDL_Quit` if the probe started them. Main thread, from terminate. Idempotent.
pub fn shutdown() {
    imp::shutdown();
}

/// The probe's state is process-global (SDL, SDL_ttf, the first failure, the one-shot result line), so
/// the unit tests that run it take this lock for their whole body. It also puts the state back to
/// "never run".
#[cfg(test)]
pub(crate) fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    shutdown();
    *FIRST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    RESULT_WRITTEN.store(false, Ordering::Release);
    g
}

#[cfg(plx_sdl)]
mod imp {
    use super::*;
    use crate::sdl::{self, Color, Font, Surface};
    use std::ffi::c_void;
    use std::sync::atomic::AtomicU32;
    use std::time::Instant;

    static TIMER_FIRED: AtomicU32 = AtomicU32::new(0);
    static SDL_STARTED: AtomicBool = AtomicBool::new(false);
    static TTF_STARTED: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn timer_cb(_interval: u32, _param: *mut c_void) -> u32 {
        TIMER_FIRED.fetch_add(1, Ordering::SeqCst);
        0 // one-shot
    }

    /// The SDL half of the probe, one logged step after another. A failed step is recorded and, where
    /// the later steps do not depend on it, the probe carries on, so one run reports as much as it can.
    pub fn probe(res_dir: Option<&str>) -> Option<Label> {
        plog!("H5 probe: static SDL2 + SDL2_ttf inside libplx.so; built against {}", env!("PLX_SDL_PINS"));
        plog!("H5 text {:?}, font {FONT_FILE} at {FONT_PX}px, res dir {:?}", TEXT, res_dir);
        let (v, rev) = sdl::version();
        plog!("H5 sdl: runtime SDL {v}, revision {rev:?}");

        // SDL_Init's event subsystem installs SIGINT/SIGTERM handlers unless told not to; the .NET
        // runtime and the Tizen app framework own those signals.
        let hint = sdl::set_hint(sdl::HINT_NO_SIGNAL_HANDLERS, c"1");
        plog!(
            "H5 sdl: SDL_SetHint(SDL_NO_SIGNAL_HANDLERS=1) -> {hint}, reads back {:?}",
            sdl::get_hint(sdl::HINT_NO_SIGNAL_HANDLERS)
        );

        plog!("H5 sdl: calling SDL_Init(TIMER | EVENTS)");
        match sdl::init(sdl::INIT_TIMER | sdl::INIT_EVENTS) {
            Ok(()) => {
                SDL_STARTED.store(true, Ordering::Release);
                let was = sdl::was_init(sdl::INIT_TIMER | sdl::INIT_EVENTS);
                plog!("H5 sdl: SDL_Init -> 0 (OK); SDL_WasInit(TIMER|EVENTS)=0x{was:x}");
                timer_and_events();
            }
            Err(e) => fail("SDL_Init", &e),
        }

        plog!("H5 ttf: calling TTF_Init");
        match sdl::ttf_init() {
            Ok(()) => {
                TTF_STARTED.store(true, Ordering::Release);
                let (ft, hb) = sdl::ttf_backend_versions();
                plog!(
                    "H5 ttf: TTF_Init -> 0 (OK); SDL_ttf {} (TTF_WasInit={}), FreeType {}.{}.{}, HarfBuzz {}.{}.{} (0.0.0 = compiled out)",
                    sdl::ttf_linked_version(),
                    sdl::ttf_was_init(),
                    ft.0, ft.1, ft.2, hb.0, hb.1, hb.2
                );
            }
            Err(e) => {
                fail("TTF_Init", &e);
                return None;
            }
        }

        let Some(res) = res_dir.map(|d| d.trim_end_matches('/')).filter(|d| !d.is_empty()) else {
            fail("font-path", "the host gave no resource directory");
            return None;
        };
        let path = format!("{res}/{FONT_FILE}");
        match std::fs::metadata(&path) {
            Ok(m) => plog!("H5 ttf: font file {path} exists, {} bytes", m.len()),
            Err(e) => {
                fail("font-file", &format!("{path}: {e}"));
                return None;
            }
        }
        plog!("H5 ttf: calling TTF_OpenFont({path}, {FONT_PX})");
        let font = match Font::open(&path, FONT_PX) {
            Ok(f) => f,
            Err(e) => {
                fail("TTF_OpenFont", &e);
                return None;
            }
        };
        plog!(
            "H5 ttf: TTF_OpenFont -> ok; family {:?} style {:?} height {}px",
            font.family(),
            font.style(),
            font.height()
        );

        let missing: Vec<char> = {
            let mut seen = Vec::new();
            for c in TEXT.chars().filter(|c| !c.is_whitespace()) {
                if !seen.contains(&c) {
                    seen.push(c);
                }
            }
            seen.into_iter().filter(|&c| !font.has_glyph(c)).collect()
        };
        if missing.is_empty() {
            plog!("H5 ttf: TTF_GlyphIsProvided32: every character of the text is in the font");
        } else {
            fail("glyphs", &format!("the font lacks {missing:?}"));
        }

        match font.size_utf8(TEXT) {
            Ok((w, h)) => plog!("H5 ttf: TTF_SizeUTF8 -> {w}x{h}"),
            Err(e) => {
                fail("TTF_SizeUTF8", &e);
                return None;
            }
        }

        plog!("H5 ttf: calling TTF_RenderUTF8_Blended");
        let t0 = Instant::now();
        let mut text = match font.render_blended(TEXT, Color { r: 255, g: 255, b: 255, a: 255 }) {
            Ok(s) => s,
            Err(e) => {
                fail("TTF_RenderUTF8_Blended", &e);
                return None;
            }
        };
        plog!(
            "H5 ttf: TTF_RenderUTF8_Blended -> SDL_Surface {}x{} pitch {} format {} in {:.1} ms",
            text.width(),
            text.height(),
            text.pitch(),
            sdl::pixel_format_name(text.format()),
            t0.elapsed().as_secs_f64() * 1000.0
        );
        if text.format() != sdl::PIXELFORMAT_ARGB8888 {
            fail("surface-format", &format!("expected ARGB8888, got 0x{:08x}", text.format()));
            return None;
        }

        let label = match compose(&mut text) {
            Ok(l) => l,
            Err(e) => {
                fail("compose", &e);
                return None;
            }
        };
        drop(text);
        drop(font);
        plog!("H5 sdl: font closed, surfaces freed; label {}x{} RGBA ready for the GL stage", label.width, label.height);
        if super::FIRST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            // A non-fatal step failed earlier (e.g. SDL_Init): the label is still worth drawing.
            plog!("H5 sdl: stage 1 finished WITH failures (see the FAILED lines above)");
        } else {
            plog!("H5 sdl: stage 1 finished OK");
        }
        Some(label)
    }

    /// A short timer and event-queue exercise: the subsystems the app would use besides video.
    fn timer_and_events() {
        TIMER_FIRED.store(0, Ordering::SeqCst);
        let t0 = sdl::ticks_ms();
        let id = sdl::add_timer(20, timer_cb, std::ptr::null_mut());
        let added = sdl::ticks_ms().saturating_sub(t0);
        // Poll for the callback instead of one fixed sleep: starting SDL's timer thread can take much
        // longer than the 20 ms interval on a busy or slow machine (measured >100 ms on the host).
        let mut waited = 0u64;
        while TIMER_FIRED.load(Ordering::SeqCst) == 0 && waited < 1000 {
            sdl::delay_ms(5);
            waited = sdl::ticks_ms().saturating_sub(t0);
        }
        let fired = TIMER_FIRED.load(Ordering::SeqCst);
        match id {
            Ok(id) => {
                // A one-shot timer that already fired is gone: removing it reports false.
                let removed = sdl::remove_timer(id);
                plog!(
                    "H5 sdl: SDL_AddTimer(20 ms) id {id} returned after {added} ms (it starts SDL's timer thread); the callback ran {fired}x by {waited} ms on SDL_GetTicks64; SDL_RemoveTimer -> {removed}"
                );
                if fired != 1 {
                    fail("SDL_AddTimer", &format!("the 20 ms timer's callback had run {fired} times after {waited} ms, expected once"));
                }
            }
            Err(e) => fail("SDL_AddTimer", &e),
        }
        let queued = sdl::pump_and_drain_events();
        plog!("H5 sdl: SDL_PumpEvents/SDL_PollEvent drained {queued} events (0 expected without video or input)");
    }

    /// Put the white text on an opaque dark plate and return tightly packed RGBA.
    fn compose(text: &mut Surface) -> Result<Label, String> {
        let (w, h) = (text.width() + 2 * PAD, text.height() + 2 * PAD);
        let mut plate = Surface::new_rgb(w, h, sdl::PIXELFORMAT_ARGB8888)?;
        plog!("H5 sdl: SDL_CreateRGBSurfaceWithFormat -> plate {w}x{h} ARGB8888");
        plate.fill(20, 20, 26, 255)?;
        text.set_blend(sdl::BLENDMODE_BLEND)?;
        text.blit_onto(&mut plate, PAD, PAD)?;
        plog!("H5 sdl: SDL_FillRect + SDL_SetSurfaceBlendMode(BLEND) + SDL_UpperBlit of the text onto the plate: ok");

        let rgba_surface = plate.convert(sdl::PIXELFORMAT_ABGR8888)?;
        plog!(
            "H5 sdl: SDL_ConvertSurfaceFormat -> {} pitch {}",
            sdl::pixel_format_name(rgba_surface.format()),
            rgba_surface.pitch()
        );
        let pitch = rgba_surface.pitch() as usize;
        let row = w as usize * 4;
        let bytes = rgba_surface.bytes();
        if pitch < row || bytes.len() < pitch * h as usize {
            return Err(format!("converted surface is smaller than expected (pitch {pitch}, {} bytes)", bytes.len()));
        }
        let mut rgba = Vec::with_capacity(row * h as usize);
        for y in 0..h as usize {
            rgba.extend_from_slice(&bytes[y * pitch..y * pitch + row]);
        }
        // SDL's software BLEND blit of an ARGB text over an opaque ARGB plate leaves alpha at 254 on the
        // anti-aliased pixels (measured on the host test). The plate is opaque by construction and the
        // GL stage draws it without blending, so make that exact instead of leaving a 1/255 hole.
        let soft = rgba.chunks_exact(4).filter(|p| p[3] != 255).count();
        for p in rgba.chunks_exact_mut(4) {
            p[3] = 255;
        }
        plog!("H5 sdl: SDL_UpperBlit left alpha < 255 on {soft} anti-aliased pixels; set to 255 (the plate is opaque)");
        // Sanity: the plate corner is the plate colour, fully opaque; and white ink exists.
        let px = |x: usize, y: usize| &rgba[(y * w as usize + x) * 4..][..4];
        let corner = px(0, 0).to_vec();
        let ink = rgba.chunks_exact(4).filter(|p| p[0] > 160 && p[1] > 160 && p[2] > 160).count();
        let total = (w * h) as usize;
        plog!(
            "H5 sdl: RGBA check: corner pixel {corner:?} (expect [20, 20, 26, 255]), bright ink pixels {ink} of {total}"
        );
        if corner != [20, 20, 26, 255] {
            return Err(format!("plate corner is {corner:?}, expected [20, 20, 26, 255] (byte order or fill wrong)"));
        }
        if ink < 200 {
            return Err(format!("only {ink} bright pixels: the text did not reach the plate"));
        }
        Ok(Label { rgba, width: w, height: h })
    }

    pub fn shutdown() {
        if TTF_STARTED.swap(false, Ordering::AcqRel) {
            sdl::ttf_quit();
            plog!("H5 ttf: TTF_Quit done (TTF_WasInit={})", sdl::ttf_was_init());
        }
        if SDL_STARTED.swap(false, Ordering::AcqRel) {
            sdl::quit();
            plog!("H5 sdl: SDL_Quit done");
        }
    }
}

#[cfg(not(plx_sdl))]
mod imp {
    use super::*;

    pub fn probe(_res_dir: Option<&str>) -> Option<Label> {
        plog!("H5 probe: this libplx.so was built WITHOUT static SDL (PLX_SDL_PREFIX unset at build time); probe skipped");
        fail("build", "built without SDL: rebuild with tizen/hello/build.sh");
        None
    }

    pub fn shutdown() {}
}

#[cfg(all(test, plx_sdl))]
mod tests {
    use super::*;
    use crate::sdl;

    /// `tizen/hello/plx` -> the repository's `pkg/` (where the app's font lives; `build.sh` copies it
    /// into the package's `res/`).
    fn pkg_dir() -> String {
        format!("{}/../../../pkg", env!("CARGO_MANIFEST_DIR"))
    }

    /// The whole SDL + TTF path on the host with no GL and no window: static SDL2/SDL2_ttf render the
    /// label's UTF-8 text from the shipped font, and the RGBA bytes the GL stage would upload come out
    /// right. Only runs where `PLX_SDL_PREFIX` pointed `build.rs` at a host build
    /// (`tizen/hello/build.sh --host-test`). `PLX_H5_DUMP=/path/label.bmp` also writes the label for a
    /// visual check.
    #[test]
    fn label_renders_to_rgba_with_static_sdl_and_the_shipped_font() {
        let _g = test_guard();
        let label = run(Some(&pkg_dir())).expect("probe produced a label; the FAILED lines above say why");
        assert!(label.width > 2 * PAD + 200, "label width {}", label.width);
        assert!(label.height > 2 * PAD + FONT_PX / 2, "label height {}", label.height);
        assert_eq!(label.rgba.len(), (label.width * label.height * 4) as usize);
        if let Ok(path) = std::env::var("PLX_H5_DUMP") {
            dump_bmp(&label, &path);
        }
        assert_eq!(&label.rgba[..4], &[20, 20, 26, 255], "plate corner, byte order R G B A");
        // Opaque plate: every alpha is 255 (the blit of the AA text onto it keeps the plate opaque).
        assert!(label.rgba.chunks_exact(4).all(|p| p[3] == 255), "a pixel is not opaque");
        // The label has both plate and ink, and the Cyrillic half (right of the middle) has ink too.
        let (w, h) = (label.width as usize, label.height as usize);
        let ink_in = |x0: usize, x1: usize| {
            (0..h)
                .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                .filter(|&(x, y)| label.rgba[(y * w + x) * 4] > 160)
                .count()
        };
        assert!(ink_in(0, w / 2) > 100, "no ink in the left half (Latin + check mark)");
        assert!(ink_in(w / 2, w) > 100, "no ink in the right half (Cyrillic)");
        // The run above must not have recorded a failure.
        assert_eq!(*FIRST_FAILURE.lock().unwrap(), None);

        shutdown();
        assert!(!sdl::ttf_was_init(), "TTF_Quit left SDL_ttf initialised");
        assert_eq!(sdl::was_init(sdl::INIT_TIMER | sdl::INIT_EVENTS), 0, "SDL_Quit left subsystems initialised");
    }

    /// A missing font is reported, not a crash, and names the file.
    #[test]
    fn a_missing_font_fails_cleanly() {
        let _g = test_guard();
        sdl::init(sdl::INIT_TIMER | sdl::INIT_EVENTS).unwrap();
        sdl::ttf_init().unwrap();
        let err = sdl::Font::open("/nonexistent/appfont.ttf", 20).err().expect("open must fail");
        assert!(err.to_lowercase().contains("couldn't open") || err.to_lowercase().contains("open"), "{err}");
        sdl::ttf_quit();
        sdl::quit();
    }

    /// 32-bit BMP of the RGBA label, so a human can look at it (`sips -s format png` converts).
    fn dump_bmp(label: &Label, path: &str) {
        let (w, h) = (label.width as u32, label.height as u32);
        let mut out = Vec::new();
        let size = 54 + w * h * 4;
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&h.to_le_bytes()); // positive height: bottom-up rows
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&[0; 24]);
        for y in (0..h as usize).rev() {
            for x in 0..w as usize {
                let p = &label.rgba[(y * w as usize + x) * 4..][..4];
                out.extend_from_slice(&[p[2], p[1], p[0], p[3]]); // BGRA
            }
        }
        std::fs::write(path, out).unwrap();
    }
}

#[cfg(all(test, not(plx_sdl)))]
mod tests {
    /// Without SDL linked, the probe reports that and does not pretend.
    #[test]
    fn without_sdl_the_probe_is_skipped_loudly() {
        let _g = super::test_guard();
        assert!(super::run(Some("/nowhere")).is_none());
        assert!(super::FIRST_FAILURE.lock().unwrap().as_deref().unwrap().starts_with("build:"));
    }
}
