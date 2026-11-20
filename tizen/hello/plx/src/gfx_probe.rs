//! Probe H7 (step 8 "gfx"): does the application's real GLES2 renderer, the `plx_gfx` crate, work on this
//! set's GL (on the Tizen emulator: YaGL, GLES 2.0)?
//!
//! Runs on the render thread, inside the EGL context step 1 already made current, over the live frames:
//! it initialises `plx_gfx` as `rust-modules/src/app/boot.rs` does (`init_gl`, `text::init_text`,
//! `init_image`, `init_blur`), then draws a filled rounded rect and a line of text through `plx_gfx`'s
//! own rect and text paths in the screen's bottom-right corner, every frame, over the diagnostics panel.
//! The panel's `8 gfx` line is the result: the init result, how many shader pairs compiled and linked,
//! `glGetError` after init, after the frame and after the swap, and any GL extension the renderer wants
//! that this GL does not list.
//!
//! **Three things this has to get around, none of them in `rust-modules/`:**
//!
//! 1. *The GL symbols.* `plx_gfx` links `gl*` at build time; `libplx.so` may not name libGLESv2. They
//!    are forwarded to the library step 1 opened ([`crate::gl_shim`]).
//! 2. *`plx_gfx` has no non-exiting entry.* Its required shaders go through `gfx::link_program` ->
//!    `gfx_compile` -> `plx_base::process::exit_now(1)` (gfx.rs, `gfx_compile`), and `init_gl` exits the
//!    same way when the base program does not link. `exit_now` is `_exit`: no destructor, no panic to
//!    catch, the .NET host dies with it. `try_compile`, `link_optional_program` and `link_shaders` are
//!    private. So [`preflight`] compiles and links every shader pair the init functions pass to
//!    `link_program`, from the very same `include_str!` sources, WITHOUT exiting, and `plx_gfx` is
//!    entered only when all of them pass. (The table is checked against `gfx.rs` / `text.rs` by a test, so
//!    a shader the renderer adds or re-composes fails this build's tests instead of silently escaping the
//!    preflight.)
//! 3. *The font.* `plx_gfx` opens `<dir of the running executable>/appfont.ttf`; here that executable is
//!    the .NET launcher. The package's font is in `res/` (the H5 probe's, the app's own Inter), so on Linux
//!    `build.rs` links with `--wrap=TTF_OpenFont` and [`__wrap_TTF_OpenFont`] retries a failed
//!    `appfont*.ttf` open from `res/`.
//!
//! Everything GL-touching sits behind [`Env`], so the state machine ([`Probe`]), the report and the
//! preflight are unit-tested on a development machine with no GL; what a real driver answers is exactly
//! what only the TV (or the emulator) can say.

use crate::dl::symtable;
use crate::log::plog;
use crate::panel::{self, Step};
use std::ffi::{c_char, c_void};
#[cfg(feature = "gfx")]
use std::ffi::c_int;
#[cfg(all(feature = "gfx", target_os = "linux"))]
use std::ffi::CString;
use std::panic::{catch_unwind, AssertUnwindSafe};

// --- GL constants (Khronos headers) -----------------------------------------------------------------
const GL_VERSION: u32 = 0x1F02;
const GL_EXTENSIONS: u32 = 0x1F03;
const GL_SHADING_LANGUAGE_VERSION: u32 = 0x8B8C;
const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_ARRAY_BUFFER: u32 = 0x8892;
#[cfg_attr(not(feature = "gfx"), allow(dead_code))]
const GL_ARRAY_BUFFER_BINDING: u32 = 0x8894;
const GL_FLOAT: u32 = 0x1406;
const GL_BLEND: u32 = 0x0BE2;
const GL_DITHER: u32 = 0x0BD0;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_TEXTURE0: u32 = 0x84C0;
const GL_SRC_ALPHA: u32 = 0x0302;
const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
const GL_ONE: u32 = 1;

/// The render-loop frame (frames presented so far) at which the probe starts. The panel's own first frame
/// and the H5 result come first.
pub const START_FRAME: u64 = 2;
/// Longest panel line the probe writes. The panel's own cap for one value is 110 characters
/// (`panel::clean`); the label before it (`8 gfx: `) is outside that. 108 leaves the `...` mark of a cut
/// inside the cap, and `panel_render`'s fit test checks the two lines still fit the plate.
const LINE_MAX: usize = 108;

symtable! {
    /// The core GLES2 entry points the probe itself calls, loaded from the opened libGLESv2. These are
    /// the preflight's compile and link, the strings it reads, and the state it saves and restores around
    /// `plx_gfx`'s draws. (Separate from `plx_gfx`'s own calls, which go through [`crate::gl_shim`].)
    #[cfg_attr(not(feature = "gfx"), allow(dead_code))]
    pub struct ProbeSyms {
        required {
            get_string: unsafe extern "C" fn(u32) -> *const u8 = "glGetString",
            get_integerv: unsafe extern "C" fn(u32, *mut i32) = "glGetIntegerv",
            get_error: unsafe extern "C" fn() -> u32 = "glGetError",
            create_shader: unsafe extern "C" fn(u32) -> u32 = "glCreateShader",
            shader_source: unsafe extern "C" fn(u32, i32, *const *const c_char, *const i32) = "glShaderSource",
            compile_shader: unsafe extern "C" fn(u32) = "glCompileShader",
            get_shaderiv: unsafe extern "C" fn(u32, u32, *mut i32) = "glGetShaderiv",
            get_shader_info_log: unsafe extern "C" fn(u32, i32, *mut i32, *mut c_char) = "glGetShaderInfoLog",
            delete_shader: unsafe extern "C" fn(u32) = "glDeleteShader",
            create_program: unsafe extern "C" fn() -> u32 = "glCreateProgram",
            attach_shader: unsafe extern "C" fn(u32, u32) = "glAttachShader",
            bind_attrib_location: unsafe extern "C" fn(u32, u32, *const c_char) = "glBindAttribLocation",
            link_program: unsafe extern "C" fn(u32) = "glLinkProgram",
            get_programiv: unsafe extern "C" fn(u32, u32, *mut i32) = "glGetProgramiv",
            get_program_info_log: unsafe extern "C" fn(u32, i32, *mut i32, *mut c_char) = "glGetProgramInfoLog",
            delete_program: unsafe extern "C" fn(u32) = "glDeleteProgram",
            bind_buffer: unsafe extern "C" fn(u32, u32) = "glBindBuffer",
            enable: unsafe extern "C" fn(u32) = "glEnable",
            disable: unsafe extern "C" fn(u32) = "glDisable",
            blend_func_separate: unsafe extern "C" fn(u32, u32, u32, u32) = "glBlendFuncSeparate",
            active_texture: unsafe extern "C" fn(u32) = "glActiveTexture",
            enable_vertex_attrib_array: unsafe extern "C" fn(u32) = "glEnableVertexAttribArray",
            disable_vertex_attrib_array: unsafe extern "C" fn(u32) = "glDisableVertexAttribArray",
            vertex_attrib_pointer: unsafe extern "C" fn(u32, i32, u32, u8, i32, *const c_void) = "glVertexAttribPointer",
            viewport: unsafe extern "C" fn(i32, i32, i32, i32) = "glViewport",
        }
        optional {}
    }
}

// --- the shader pairs init_gl / init_text / init_image / init_blur pass to `link_program` ------------

/// One required vertex + fragment pair, composed exactly as `gfx.rs` / `text.rs` compose it. A required
/// pair is one the renderer EXITS THE PROCESS on if it does not compile (see the module doc).
pub struct Pair {
    /// Short name for the panel: the `init_*` function and the program.
    pub name: &'static str,
    /// The NUL-terminated GLSL sources.
    pub vs: &'static str,
    pub fs: &'static str,
    /// `(const name, the macro call that defines it)` as it reads in `gfx.rs` / `text.rs`, for the test that
    /// keeps this table honest (read by tests only).
    #[cfg_attr(not(test), allow(dead_code))]
    pub vs_decl: (&'static str, &'static str),
    #[cfg_attr(not(test), allow(dead_code))]
    pub fs_decl: (&'static str, &'static str),
}

macro_rules! sh {
    (plain $file:literal) => {
        concat!(include_str!(concat!("../../../../rust-modules/gfx/src/shaders/", $file)), "\0")
    };
    (prefix $prefix:literal, $file:literal) => {
        concat!($prefix, include_str!(concat!("../../../../rust-modules/gfx/src/shaders/", $file)), "\0")
    };
    (dithered $file:literal) => {
        concat!(
            include_str!("../../../../rust-modules/gfx/src/shaders/dither.glsl"),
            include_str!(concat!("../../../../rust-modules/gfx/src/shaders/", $file)),
            "\0"
        )
    };
    (undithered $file:literal) => {
        concat!(
            include_str!("../../../../rust-modules/gfx/src/shaders/dither_stub.glsl"),
            include_str!(concat!("../../../../rust-modules/gfx/src/shaders/", $file)),
            "\0"
        )
    };
    (vs_dithered $file:literal) => {
        concat!("#define PLX_DITHER_NC\n", include_str!(concat!("../../../../rust-modules/gfx/src/shaders/", $file)), "\0")
    };
}

const VS_SRC: (&str, (&str, &str)) =
    (sh!(plain "vs_src.vert"), ("VS_SRC", r#"glsl!("shaders/vs_src.vert")"#));
const FS_SRC: (&str, (&str, &str)) =
    (sh!(plain "fs_src.frag"), ("FS_SRC", r#"glsl!("shaders/fs_src.frag")"#));
const VS_AMBIENT_DITHERED: (&str, (&str, &str)) = (
    sh!(prefix "#define PLX_DITHER_NC\n#define PLX_WASH_INK\n#define PLX_SCRIM\n", "vs_ambient.vert"),
    (
        "VS_AMBIENT_DITHERED",
        r##"glsl!("shaders/vs_ambient.vert", "#define PLX_DITHER_NC\n#define PLX_WASH_INK\n#define PLX_SCRIM\n")"##,
    ),
);
const FS_AMBIENT: (&str, (&str, &str)) =
    (sh!(dithered "fs_ambient.frag"), ("FS_AMBIENT", r#"glsl_dithered!("shaders/fs_ambient.frag")"#));
const VS_SCRIM_LAYER: (&str, (&str, &str)) = (
    sh!(prefix "#define PLX_SCRIM\n#define PLX_SCRIM_LAYER\n", "vs_ambient.vert"),
    ("VS_SCRIM_LAYER", r##"glsl!("shaders/vs_ambient.vert", "#define PLX_SCRIM\n#define PLX_SCRIM_LAYER\n")"##),
);
const FS_AMBIENT_PLAIN: (&str, (&str, &str)) =
    (sh!(undithered "fs_ambient.frag"), ("FS_AMBIENT_PLAIN", r#"glsl_undithered!("shaders/fs_ambient.frag")"#));
const VS_AMBIENT: (&str, (&str, &str)) =
    (sh!(plain "vs_ambient.vert"), ("VS_AMBIENT", r#"glsl!("shaders/vs_ambient.vert")"#));
const FS_SHADOW: (&str, (&str, &str)) =
    (sh!(plain "fs_shadow.frag"), ("FS_SHADOW", r#"glsl!("shaders/fs_shadow.frag")"#));
const FS_FLAT: (&str, (&str, &str)) =
    (sh!(plain "fs_flat.frag"), ("FS_FLAT", r#"glsl!("shaders/fs_flat.frag")"#));
const VS_SRC_DITHERED: (&str, (&str, &str)) =
    (sh!(vs_dithered "vs_src.vert"), ("VS_SRC_DITHERED", r#"glsl_vs_dithered!("shaders/vs_src.vert")"#));
const FS_FIELD: (&str, (&str, &str)) =
    (sh!(dithered "fs_field.frag"), ("FS_FIELD", r#"glsl_dithered!("shaders/fs_field.frag")"#));
const FS_FIELD_PANEL: (&str, (&str, &str)) =
    (sh!(dithered "fs_field_panel.frag"), ("FS_FIELD_PANEL", r#"glsl_dithered!("shaders/fs_field_panel.frag")"#));
const VS_TEXT: (&str, (&str, &str)) =
    (sh!(plain "vs_text.vert"), ("VS_TEXT", r#"crate::gfx::glsl!("shaders/vs_text.vert")"#));
const FS_TEXT: (&str, (&str, &str)) =
    (sh!(plain "fs_text.frag"), ("FS_TEXT", r#"crate::gfx::glsl!("shaders/fs_text.frag")"#));
const FS_TEXT_FADE: (&str, (&str, &str)) =
    (sh!(plain "fs_text_fade.frag"), ("FS_TEXT_FADE", r#"crate::gfx::glsl!("shaders/fs_text_fade.frag")"#));
const FS_TEXT_INK: (&str, (&str, &str)) =
    (sh!(plain "fs_text_ink.frag"), ("FS_TEXT_INK", r#"crate::gfx::glsl!("shaders/fs_text_ink.frag")"#));
const VS_IMG: (&str, (&str, &str)) =
    (sh!(plain "vs_img.vert"), ("VS_IMG", r#"glsl!("shaders/vs_img.vert")"#));
const FS_IMG: (&str, (&str, &str)) =
    (sh!(plain "fs_img.frag"), ("FS_IMG", r#"glsl!("shaders/fs_img.frag")"#));
const FS_BLUR: (&str, (&str, &str)) =
    (sh!(plain "fs_blur.frag"), ("FS_BLUR", r#"glsl!("shaders/fs_blur.frag")"#));
const VS_IMG_DITHERED: (&str, (&str, &str)) =
    (sh!(vs_dithered "vs_img.vert"), ("VS_IMG_DITHERED", r#"glsl_vs_dithered!("shaders/vs_img.vert")"#));
const FS_GLASS: (&str, (&str, &str)) =
    (sh!(dithered "fs_glass.frag"), ("FS_GLASS", r#"glsl_dithered!("shaders/fs_glass.frag")"#));

const fn pair(name: &'static str, vs: (&'static str, (&'static str, &'static str)), fs: (&'static str, (&'static str, &'static str))) -> Pair {
    Pair { name, vs: vs.0, fs: fs.0, vs_decl: vs.1, fs_decl: fs.1 }
}

/// Every pair that `init_gl`, `init_text`, `init_image` and `init_blur` pass to `gfx::link_program` (the
/// exit-on-compile-failure entry), in the order they are made. The `link_optional_program` pairs (the
/// rounded-clip variants, the ramp, the stills, the frost bake, the straighten programs) degrade on a
/// failure and are left out on purpose: they cannot exit.
pub const PAIRS: &[Pair] = &[
    pair("init_gl base rect", VS_SRC, FS_SRC),
    pair("init_gl ambient", VS_AMBIENT_DITHERED, FS_AMBIENT),
    pair("init_gl scrim layer", VS_SCRIM_LAYER, FS_AMBIENT_PLAIN),
    pair("init_gl ambient plain", VS_AMBIENT, FS_AMBIENT_PLAIN),
    pair("init_gl shadow", VS_SRC, FS_SHADOW),
    pair("init_gl flat", VS_SRC, FS_FLAT),
    pair("init_gl field", VS_SRC_DITHERED, FS_FIELD),
    pair("init_gl field panel", VS_SRC_DITHERED, FS_FIELD_PANEL),
    pair("init_text text", VS_TEXT, FS_TEXT),
    pair("init_text fade", VS_TEXT, FS_TEXT_FADE),
    pair("init_text ink", VS_TEXT, FS_TEXT_INK),
    pair("init_image image", VS_IMG, FS_IMG),
    pair("init_blur blur", VS_IMG, FS_BLUR),
    pair("init_blur glass", VS_IMG_DITHERED, FS_GLASS),
];

// --- the preflight -------------------------------------------------------------------------------

/// Compile and link, as `gfx::try_compile` / `link_shaders` do, but returning the driver's complaint
/// instead of exiting.
pub trait ShaderGl {
    /// Compile one NUL-terminated GLSL source. `Err` is the driver's info log (or why there is none).
    fn compile(&mut self, fragment: bool, src: &'static str) -> Result<u32, String>;
    /// Attach both, bind `a_pos` to attribute 0 (what `link_shaders` does) and link.
    fn link(&mut self, vs: u32, fs: u32) -> Result<u32, String>;
    /// Delete the shaders and, when there is one, the program.
    fn delete(&mut self, vs: u32, fs: u32, program: Option<u32>);
}

/// The outcome of one pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairResult {
    pub name: &'static str,
    pub outcome: Result<(), String>,
}

/// Compile and link every pair, deleting what it made. Never exits, never panics on a driver answer.
pub fn preflight(gl: &mut dyn ShaderGl, pairs: &[Pair]) -> Vec<PairResult> {
    pairs
        .iter()
        .map(|p| {
            let outcome = (|| {
                let vs = gl.compile(false, p.vs).map_err(|e| format!("vertex: {e}"))?;
                let fs = match gl.compile(true, p.fs) {
                    Ok(fs) => fs,
                    Err(e) => {
                        gl.delete(vs, 0, None);
                        return Err(format!("fragment: {e}"));
                    }
                };
                let program = gl.link(vs, fs);
                match program {
                    Ok(program) => {
                        gl.delete(vs, fs, Some(program));
                        Ok(())
                    }
                    Err(e) => {
                        gl.delete(vs, fs, None);
                        Err(format!("link: {e}"))
                    }
                }
            })();
            PairResult { name: p.name, outcome }
        })
        .collect()
}

/// The driver's answer to a shader or program, first line only, for the panel.
fn first_line(s: &str) -> String {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("(empty info log)").to_string()
}

/// [`ShaderGl`] over the opened libGLESv2.
pub struct RealShaderGl<'a>(pub &'a ProbeSyms);

impl RealShaderGl<'_> {
    fn log_text(buf: &[c_char], n: i32) -> String {
        let n = (n.max(0) as usize).min(buf.len());
        let bytes: Vec<u8> = buf[..n].iter().map(|&c| c as u8).collect();
        first_line(&String::from_utf8_lossy(&bytes))
    }
}

impl ShaderGl for RealShaderGl<'_> {
    fn compile(&mut self, fragment: bool, src: &'static str) -> Result<u32, String> {
        let s = self.0;
        let kind = if fragment { GL_FRAGMENT_SHADER } else { GL_VERTEX_SHADER };
        // SAFETY (whole function): the render thread's context is current; `src` is NUL-terminated
        // static text (the `sh!` macros append the terminator); the log buffer's size is passed.
        let shader = unsafe { (s.create_shader)(kind) };
        if shader == 0 {
            return Err("glCreateShader returned 0".to_string());
        }
        let ptr = src.as_ptr() as *const c_char;
        let mut ok = 0i32;
        unsafe {
            (s.shader_source)(shader, 1, &ptr, std::ptr::null());
            (s.compile_shader)(shader);
            (s.get_shaderiv)(shader, GL_COMPILE_STATUS, &mut ok);
        }
        if ok == 0 {
            let mut buf = [0 as c_char; 512];
            let mut n = 0i32;
            unsafe {
                (s.get_shader_info_log)(shader, buf.len() as i32, &mut n, buf.as_mut_ptr());
                (s.delete_shader)(shader);
            }
            return Err(Self::log_text(&buf, n));
        }
        Ok(shader)
    }

    fn link(&mut self, vs: u32, fs: u32) -> Result<u32, String> {
        let s = self.0;
        // SAFETY: as `compile`.
        let program = unsafe { (s.create_program)() };
        if program == 0 {
            return Err("glCreateProgram returned 0".to_string());
        }
        let mut ok = 0i32;
        unsafe {
            (s.attach_shader)(program, vs);
            (s.attach_shader)(program, fs);
            (s.bind_attrib_location)(program, 0, c"a_pos".as_ptr());
            (s.link_program)(program);
            (s.get_programiv)(program, GL_LINK_STATUS, &mut ok);
        }
        if ok == 0 {
            let mut buf = [0 as c_char; 512];
            let mut n = 0i32;
            unsafe {
                (s.get_program_info_log)(program, buf.len() as i32, &mut n, buf.as_mut_ptr());
                (s.delete_program)(program);
            }
            return Err(Self::log_text(&buf, n));
        }
        Ok(program)
    }

    fn delete(&mut self, vs: u32, fs: u32, program: Option<u32>) {
        let s = self.0;
        // SAFETY: names made by `compile` / `link` on this context; deleting name 0 is a no-op in GL.
        unsafe {
            if let Some(p) = program {
                (s.delete_program)(p);
            }
            (s.delete_shader)(vs);
            (s.delete_shader)(fs);
        }
    }
}

// --- the GL strings ---------------------------------------------------------------------------------

/// Is this `GL_SHADING_LANGUAGE_VERSION` an OpenGL ES one? The test `plx_gfx` makes itself (`gl_is_es`):
/// the ES spec's exact prefix, not a substring. An unreadable string counts as ES, as it does there.
pub fn glsl_is_es(glsl: &str) -> bool {
    glsl.is_empty() || glsl.starts_with("OpenGL ES")
}

/// What the renderer wants of the GL extension string: `(what it is for, the alternatives, what it does
/// without)`. Any one alternative satisfies the need.
const WANTED_EXTENSIONS: &[(&str, &[&str], &str)] = &[
    (
        "straighten pass reads the framebuffer in place",
        &["GL_ARM_shader_framebuffer_fetch", "GL_EXT_shader_framebuffer_fetch"],
        "copies the frame instead",
    ),
    ("GPU timing of the frame", &["GL_EXT_disjoint_timer_query"], "timing stays off"),
];

/// The needs of [`WANTED_EXTENSIONS`] that no alternative of `GL_EXTENSIONS` meets, as a short name
/// (`ARM|EXT_shader_framebuffer_fetch`). Whole space-separated tokens, never a substring.
pub fn missing_extensions(extensions: &str) -> Vec<String> {
    let have: Vec<&str> = extensions.split_ascii_whitespace().collect();
    WANTED_EXTENSIONS
        .iter()
        .filter(|(_, alts, _)| !alts.iter().any(|a| have.contains(a)))
        .map(|(_, alts, _)| short_extension(alts))
        .collect()
}

/// `GL_ARM_shader_framebuffer_fetch` + `GL_EXT_shader_framebuffer_fetch` -> `ARM|EXT_shader_framebuffer_fetch`.
fn short_extension(alts: &[&str]) -> String {
    let split = |a: &str| -> (String, String) {
        let rest = a.strip_prefix("GL_").unwrap_or(a);
        match rest.split_once('_') {
            Some((vendor, name)) => (vendor.to_string(), name.to_string()),
            None => (String::new(), rest.to_string()),
        }
    };
    let parts: Vec<(String, String)> = alts.iter().map(|a| split(a)).collect();
    let same_name = parts.windows(2).all(|w| w[0].1 == w[1].1);
    if parts.len() > 1 && same_name {
        let vendors: Vec<&str> = parts.iter().map(|(v, _)| v.as_str()).collect();
        format!("{}_{}", vendors.join("|"), parts[0].1)
    } else {
        alts.iter().map(|a| a.strip_prefix("GL_").unwrap_or(a)).collect::<Vec<_>>().join("|")
    }
}

// --- the report -------------------------------------------------------------------------------------

/// What the probe learned, in one place; [`Report::lines`] is what the panel shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// Why `plx_gfx` was not entered (the preflight failed, the GL is not ES, an entry is missing, built
    /// without it). `None` when it was entered.
    pub refusal: Option<String>,
    /// `(passed, total)` of the shader preflight, and the first failure (`name: driver's words`).
    pub shaders: Option<(usize, usize, Option<String>)>,
    /// The four init calls that ran, each with how it ended (`Err` is a panic's message).
    pub stages: Vec<(&'static str, Result<(), String>)>,
    /// `glGetError` (first nonzero of up to 16, drained) after init, after the frame's draw, after the swap.
    pub err_init: Option<u32>,
    pub err_frame: Option<u32>,
    pub err_swap: Option<u32>,
    /// The width `draw_text` returned, once a swatch was drawn. 0 = the text path drew nothing.
    pub text_px: Option<f32>,
    /// The swatch's rect was drawn without a panic.
    pub rect_drawn: bool,
    /// Extensions the renderer wants that `GL_EXTENSIONS` lacks ([`missing_extensions`]).
    pub missing_ext: Vec<String>,
    /// The package font was reached through the `TTF_OpenFont` redirect, and where `plx_gfx` first looked.
    pub font_redirected: bool,
    pub font_expected: String,
    /// Event-log lines `plx_gfx` wrote that say something failed: how many, and the first.
    pub failure_events: usize,
    pub first_failure_event: Option<String>,
    /// A draw panicked or an unbound GL forwarder was called after the swatch was up.
    pub late_fault: Option<String>,
}

fn cap(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut cut: String = s.chars().take(max.saturating_sub(3)).collect();
        cut.push_str("...");
        cut
    } else {
        s.to_string()
    }
}

/// A `glGetError` code for the panel: `0` for none (the common answer, kept short), `0x0502` otherwise,
/// `?` when it was never read.
fn err_hex(e: Option<u32>) -> String {
    match e {
        None => "?".to_string(),
        Some(0) => "0".to_string(),
        Some(e) => format!("0x{e:04x}"),
    }
}

impl Report {
    /// Did everything work: entered, the four inits ran, every shader pair linked, the swatch drew (text
    /// included), no GL error anywhere, no late fault.
    pub fn is_ok(&self) -> bool {
        self.refusal.is_none()
            && self.stages.len() == 4
            && self.stages.iter().all(|(_, r)| r.is_ok())
            && self.shaders.as_ref().is_some_and(|(ok, total, _)| ok == total)
            && self.rect_drawn
            && self.text_px.is_some_and(|w| w > 0.0)
            && [self.err_init, self.err_frame, self.err_swap].iter().all(|e| *e == Some(0))
            && self.late_fault.is_none()
    }

    /// The panel's two lines for step 8 and whether the step is green. Each line is at most [`LINE_MAX`]
    /// characters; the verdict comes first so a cut loses detail, not the answer.
    pub fn lines(&self) -> (bool, [String; 2]) {
        let shaders = match &self.shaders {
            Some((ok, total, None)) => format!("shaders {ok}/{total} compile+link"),
            Some((ok, total, Some(first))) => format!("shaders {ok}/{total} compile+link, first failure {first}"),
            None => "shaders not checked".to_string(),
        };
        let ext = if self.missing_ext.is_empty() {
            String::new()
        } else {
            format!("no GL ext: {}", self.missing_ext.join(" "))
        };
        // The font finding rides on the draw part of line 1 (the text's own result) and is shown only when
        // the redirect was what opened the font.
        let font = if self.font_redirected { " (res/ font)" } else { "" };
        let l1 = if let Some(why) = &self.refusal {
            format!("FAILED, plx_gfx not entered: {why}")
        } else {
            let bad: Vec<String> = self
                .stages
                .iter()
                .filter_map(|(n, r)| r.as_ref().err().map(|e| format!("{n} PANICKED ({})", cap(e, 30))))
                .collect();
            let stages = if bad.is_empty() && self.stages.len() == 4 {
                "init_gl+text+image+blur ran".to_string()
            } else if bad.is_empty() {
                format!("{} of 4 inits ran", self.stages.len())
            } else {
                bad.join(", ")
            };
            let draw = match (self.rect_drawn, self.text_px) {
                (true, Some(w)) if w > 0.0 => format!("rect drawn, text {w:.0}px{font}"),
                (true, Some(_)) => "rect drawn, TEXT DREW 0px".to_string(),
                (true, None) => "rect drawn".to_string(),
                (false, _) => "no swatch drawn yet".to_string(),
            };
            let verdict = if self.is_ok() { "OK" } else { "NOT OK" };
            format!("{verdict}: {stages}; {shaders}; {draw}")
        };
        // Line 2, most important first (a cut loses the tail): the GL error codes, then anything that went
        // wrong, then the extensions the renderer wanted and this GL does not list.
        let l2 = if self.refusal.is_some() {
            [shaders.clone(), ext].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join("; ")
        } else {
            let mut parts = vec![format!(
                "glGetError init/frame/swap {}/{}/{}",
                err_hex(self.err_init),
                err_hex(self.err_frame),
                err_hex(self.err_swap)
            )];
            if self.text_px == Some(0.0) {
                parts.push(format!("no font face, looked in {}", cap(&self.font_expected, 40)));
            }
            if let Some(f) = &self.late_fault {
                parts.push(f.clone());
            }
            if self.failure_events > 0 {
                parts.push(format!(
                    "{} failure line(s) in plx_gfx's log, first: {}",
                    self.failure_events,
                    cap(self.first_failure_event.as_deref().unwrap_or(""), 40)
                ));
            }
            if !ext.is_empty() {
                parts.push(ext);
            }
            parts.join("; ")
        };
        (self.is_ok(), [cap(&l1, LINE_MAX), cap(&l2, LINE_MAX)])
    }
}

/// An event-log line that says something failed (the renderer's own words: `shader compile FAILED`,
/// `base prog link FAILED`, `text prog link failed`, `TTF_Init failed`, ...).
pub fn looks_like_failure(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("failed") || l.contains("unavailable") || l.contains("cannot")
}

// --- the environment the state machine runs in -----------------------------------------------------

/// The four calls `rust-modules/src/app/boot.rs` makes, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    InitGl,
    InitText,
    InitImage,
    InitBlur,
}

impl Stage {
    const ALL: [Stage; 4] = [Stage::InitGl, Stage::InitText, Stage::InitImage, Stage::InitBlur];

    fn name(self) -> &'static str {
        match self {
            Stage::InitGl => "init_gl",
            Stage::InitText => "init_text",
            Stage::InitImage => "init_image",
            Stage::InitBlur => "init_blur",
        }
    }
}

/// The GL strings the probe reads once, with the context current.
#[derive(Clone, Debug, Default)]
pub struct GlStrings {
    pub version: String,
    pub glsl: String,
    pub extensions: String,
}

/// Everything the state machine touches outside itself, so a test can stand a fake in.
pub trait Env {
    /// Is `plx_gfx` linked into this build (`--features gfx`)?
    fn available(&self) -> bool;
    /// Forwarded GL entries the library lacks, and probe entries it lacks: either one means "do not enter".
    fn missing_gl(&self) -> Vec<String>;
    fn gl_strings(&mut self) -> GlStrings;
    fn shader_gl(&mut self) -> &mut dyn ShaderGl;
    /// First nonzero `glGetError` of up to 16 (all drained), 0 when there is none.
    fn drain_errors(&mut self) -> u32;
    /// Run one init call. A panic is the caller's to catch.
    fn init(&mut self, stage: Stage);
    /// Make `plx_gfx`'s GL state what it assumes (its buffer, attribute 0, blending, texture unit 0), draw
    /// the swatch (a rect and a line of text), and put the state back for the panel's own client-side
    /// arrays. Returns the text's width in pixels.
    fn draw_swatch(&mut self, width: i32, height: i32) -> f32;
    /// Where `plx_gfx` first looks for its font, and whether the redirect has since opened one from `res/`.
    fn font(&self) -> (String, bool);
    /// Event-log lines written since the last call (also forwarded to the UDP log).
    fn new_events(&mut self) -> Vec<String>;
    /// `(count, first name)` of calls that reached an unbound GL forwarder.
    fn unbound_calls(&self) -> (u32, Option<&'static str>);
    /// Put the step's two lines on the panel (`ok` picks its colour).
    fn publish(&mut self, ok: bool, lines: [String; 2]);
}

/// Where the probe is. Each arrow is a separate render-loop frame so the panel shows how far the probe got
/// if the next step takes the process with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for [`START_FRAME`].
    Idle,
    /// The panel says "preflighting"; the next frame does it.
    Preflight,
    /// The panel says "entering the inits"; the next frame runs them.
    Init,
    /// The inits ran; the next frame draws the swatch.
    FirstDraw,
    /// The first swatch is in the back buffer; [`Probe::presented`] closes the report.
    AwaitSwap,
    /// Reported; the swatch is drawn on every frame.
    Live,
    /// Nothing more to do (refused, or a draw panicked).
    Done,
}

pub struct Probe<E: Env> {
    env: E,
    phase: Phase,
    pub report: Report,
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".to_string())
}

impl<E: Env> Probe<E> {
    pub fn new(env: E) -> Probe<E> {
        Probe { env, phase: Phase::Idle, report: Report::default() }
    }

    /// Where the probe is (read by the tests that walk it through its phases).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    fn publish_provisional(&mut self, line1: &str, line2: &str) {
        plog!("H7 gfx: {line1} | {line2}");
        self.env.publish(true, [cap(line1, LINE_MAX), cap(line2, LINE_MAX)]);
    }

    fn publish_report(&mut self) {
        let (ok, lines) = self.report.lines();
        plog!("H7 RESULT: {} | {}", lines[0], lines[1]);
        self.env.publish(ok, lines);
    }

    /// Forward what `plx_gfx` wrote to the event log into the report (and the UDP log, via the env).
    fn collect_events(&mut self) {
        for line in self.env.new_events() {
            if looks_like_failure(&line) {
                self.report.failure_events += 1;
                self.report.first_failure_event.get_or_insert(line);
            }
        }
    }

    fn refuse(&mut self, why: String) {
        plog!("H7 gfx: plx_gfx NOT entered: {why}");
        self.report.refusal = Some(why);
        self.phase = Phase::Done;
        self.publish_report();
    }

    /// Called once per render-loop iteration, after the panel's own draw and before the swap, with the
    /// number of frames presented so far and the window size.
    pub fn frame(&mut self, frames: u64, width: i32, height: i32) {
        match self.phase {
            Phase::Idle if frames >= START_FRAME => {
                if !self.env.available() {
                    self.refuse("this build has no plx_gfx (cargo feature gfx off: build.sh --no-gfx)".to_string());
                    return;
                }
                self.publish_provisional(
                    "preflight: compiling and linking the renderer's shader pairs without exiting",
                    "plx_gfx is entered only if every required pair passes (it would exit the process otherwise)",
                );
                self.phase = Phase::Preflight;
            }
            Phase::Preflight => self.run_preflight(),
            Phase::Init => self.run_inits(),
            Phase::FirstDraw => {
                self.draw(width, height, true);
            }
            Phase::Live => {
                self.draw(width, height, false);
            }
            Phase::Idle | Phase::AwaitSwap | Phase::Done => {}
        }
    }

    fn run_preflight(&mut self) {
        let strings = self.env.gl_strings();
        plog!("H7 gfx: GL_VERSION {:?} GLSL {:?}", strings.version, strings.glsl);
        plog!("H7 gfx: GL_EXTENSIONS {}", strings.extensions);
        self.report.missing_ext = missing_extensions(&strings.extensions);
        let missing = self.env.missing_gl();
        if !missing.is_empty() {
            return self.refuse(format!("GL entries not found in libGLESv2: {}", missing.join(" ")));
        }
        if !glsl_is_es(&strings.glsl) {
            return self.refuse(format!(
                "GLSL {:?} is not OpenGL ES (plx_gfx would prepend #version 410 core)",
                cap(&strings.glsl, 30)
            ));
        }
        let results = match catch_unwind(AssertUnwindSafe(|| preflight(self.env.shader_gl(), PAIRS))) {
            Ok(r) => r,
            Err(p) => return self.refuse(format!("the shader preflight panicked: {}", cap(&panic_text(&*p), 50))),
        };
        let total = results.len();
        let passed = results.iter().filter(|r| r.outcome.is_ok()).count();
        let first = results.iter().find_map(|r| r.outcome.as_ref().err().map(|e| format!("{}: {}", r.name, cap(e, 40))));
        for r in &results {
            match &r.outcome {
                Ok(()) => plog!("H7 gfx: shader pair {:?} compiled and linked", r.name),
                Err(e) => plog!("H7 gfx: shader pair {:?} FAILED: {e}", r.name),
            }
        }
        let failed = first.is_some();
        self.report.shaders = Some((passed, total, first));
        let _ = self.env.drain_errors();
        if failed {
            // The first failure (which pair, the driver's words) is on line 2 and in the log; line 1 keeps
            // the verdict short enough to show whole.
            return self.refuse(format!("{} of {total} shader pairs do not compile+link (init would exit)", total - passed));
        }
        self.publish_provisional(
            &format!("preflight ok: shaders {passed}/{total} compile+link; entering plx_gfx init_gl, init_text, init_image, init_blur"),
            "if the app vanished here, one of those calls exited the process (see the UDP log's last 'entering' line)",
        );
        self.phase = Phase::Init;
    }

    fn run_inits(&mut self) {
        let (expected, _) = self.env.font();
        self.report.font_expected = expected;
        for stage in Stage::ALL {
            plog!("H7 gfx: entering {}", stage.name());
            let r = catch_unwind(AssertUnwindSafe(|| self.env.init(stage)));
            let result = r.map_err(|p| panic_text(&*p));
            match &result {
                Ok(()) => plog!("H7 gfx: {} returned", stage.name()),
                Err(e) => plog!("H7 gfx: {} PANICKED: {e}", stage.name()),
            }
            self.report.stages.push((stage.name(), result));
            self.collect_events();
        }
        self.report.err_init = Some(self.env.drain_errors());
        plog!("H7 gfx: glGetError after init = {}", err_hex(self.report.err_init));
        self.phase = Phase::FirstDraw;
        let (_, lines) = self.report.lines();
        self.publish_provisional(&lines[0], "inits done; the swatch (a rect and a line of text) is drawn on the next frame");
    }

    fn draw(&mut self, width: i32, height: i32, first: bool) {
        if !self.report.stages.iter().all(|(_, r)| r.is_ok()) {
            // An init panicked: drawing through half-initialised statics is not worth the risk.
            self.report.err_frame = Some(0);
            self.report.err_swap = Some(0);
            self.phase = Phase::Done;
            self.collect_events();
            return self.publish_report();
        }
        match catch_unwind(AssertUnwindSafe(|| self.env.draw_swatch(width, height))) {
            Ok(px) => {
                let err = self.env.drain_errors();
                if first {
                    self.report.rect_drawn = true;
                    self.report.text_px = Some(px);
                    self.report.err_frame = Some(err);
                    plog!("H7 gfx: swatch drawn, text {px:.1}px, glGetError after the draw = {}", err_hex(Some(err)));
                    self.phase = Phase::AwaitSwap;
                } else if self.report.late_fault.is_none() {
                    let (n, name) = self.env.unbound_calls();
                    if err != 0 || n > 0 {
                        let fault = if n > 0 { format!("{n} unbound GL call(s), first {}", name.unwrap_or("?")) } else { format!("glGetError 0x{err:04x} on a later frame") };
                        plog!("H7 gfx: {fault}");
                        self.report.late_fault = Some(fault);
                        self.publish_report();
                    }
                }
            }
            Err(p) => {
                let why = panic_text(&*p);
                plog!("H7 gfx: the swatch draw PANICKED: {why}");
                self.report.late_fault = Some(format!("draw PANICKED: {}", cap(&why, 40)));
                self.report.rect_drawn = false;
                self.report.err_frame = self.report.err_frame.or(Some(0));
                self.report.err_swap = self.report.err_swap.or(Some(0));
                self.phase = Phase::Done;
                self.publish_report();
            }
        }
    }

    /// The first frame carrying the swatch was presented: read `glGetError` once more, settle the font
    /// finding and write the result.
    pub fn presented(&mut self) {
        if self.phase != Phase::AwaitSwap {
            return;
        }
        let err = self.env.drain_errors();
        self.report.err_swap = Some(err);
        let (expected, redirected) = self.env.font();
        self.report.font_expected = expected;
        self.report.font_redirected = redirected;
        self.collect_events();
        self.phase = Phase::Live;
        self.publish_report();
    }
}

// --- the real environment --------------------------------------------------------------------------

/// What `plx_tizen_create` hands the render thread for step 8.
pub struct GfxJob {
    /// The probe's own GL entry points, or the names libGLESv2 lacked.
    pub syms: Result<ProbeSyms, Vec<&'static str>>,
    /// `gl*` forwarders ([`crate::gl_shim`]) that found no function in libGLESv2.
    pub forwarders_missing: Vec<&'static str>,
}

/// Load the probe's GL entries and bind the forwarders. Main thread, in `plx_tizen_create`, once.
pub fn job(gles: &crate::dl::Lib) -> GfxJob {
    #[cfg(feature = "gfx")]
    let forwarders_missing = {
        let missing = crate::gl_shim::install(gles);
        plog!(
            "H7 gfx: {} of {} GL forwarders bound to {}{}",
            crate::gl_shim::NAMES.len() - missing.len(),
            crate::gl_shim::NAMES.len(),
            gles.name,
            if missing.is_empty() { String::new() } else { format!("; NOT FOUND: {}", missing.join(" ")) }
        );
        missing
    };
    #[cfg(not(feature = "gfx"))]
    let forwarders_missing = Vec::new();
    GfxJob { syms: ProbeSyms::load(&[gles]), forwarders_missing }
}

/// The package's `res/` directory, for the font redirect. Set once by `plx_tizen_create`.
pub fn set_res_dir(dir: Option<&str>) {
    if let Some(d) = dir.filter(|d| !d.is_empty()) {
        let _ = RES_DIR.set(d.to_string());
    }
}

static RES_DIR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static FONT_REDIRECTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(all(feature = "gfx", target_os = "linux"))]
extern "C" {
    fn __real_TTF_OpenFont(file: *const c_char, ptsize: c_int) -> *mut c_void;
}

/// `--wrap=TTF_OpenFont` (see `build.rs`): `plx_gfx` opens `<dir of current_exe>/appfont.ttf`, which is next
/// to the .NET launcher here and holds no font. Try the path it asked for first; when that fails for one of
/// the app's two faces, retry the package's `res/` directory. Anything else is passed through unchanged.
///
/// # Safety
/// `file` is a NUL-terminated path (SDL_ttf's own contract).
#[cfg(all(feature = "gfx", target_os = "linux"))]
#[no_mangle]
pub unsafe extern "C" fn __wrap_TTF_OpenFont(file: *const c_char, ptsize: c_int) -> *mut c_void {
    let font = __real_TTF_OpenFont(file, ptsize);
    if !font.is_null() || file.is_null() {
        return font;
    }
    let asked = std::ffi::CStr::from_ptr(file).to_string_lossy().into_owned();
    let Some(name) = asked.rsplit('/').next().filter(|n| n.starts_with("appfont")) else {
        return font;
    };
    let Some(res) = RES_DIR.get() else { return font };
    let Ok(alt) = CString::new(format!("{res}/{name}")) else { return font };
    let retry = __real_TTF_OpenFont(alt.as_ptr(), ptsize);
    if !retry.is_null() {
        FONT_REDIRECTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    retry
}

/// The event log's lines not yet forwarded, from the lab ring `plx_base` keeps in memory. A cursor over a
/// ring that evicts from the front: `seen` counts lines ever consumed, `evicted` lines ever dropped.
#[derive(Default)]
pub struct RingCursor {
    seen: u64,
    evicted: u64,
}

impl RingCursor {
    /// Index into the ring's current contents (`len` records, `dropped` evicted since the last read) of
    /// the first line not yet consumed; moves the cursor past all `len`.
    pub fn advance(&mut self, len: usize, dropped: u64) -> usize {
        self.evicted += dropped;
        let first_abs = self.evicted;
        let start = self.seen.saturating_sub(first_abs).min(len as u64) as usize;
        self.seen = first_abs + len as u64;
        start
    }
}

static CURSOR: std::sync::Mutex<Option<RingCursor>> = std::sync::Mutex::new(None);

/// Forward the event-log lines written since the last call to the UDP log (`evt ...`), once each, and
/// return them. Both step 8 and step 9 read the same ring, so the cursor is process-global.
pub fn forward_events() -> Vec<String> {
    // The cursor's lock is held across `take`: `take` reports the evictions since ITS previous call, so two
    // threads interleaving take and advance would apply one thread's count to the other's snapshot.
    let mut guard = CURSOR.lock().unwrap_or_else(|e| e.into_inner());
    let (recs, dropped) = plx_base::eventlog::ring::take();
    let start = guard.get_or_insert_with(RingCursor::default).advance(recs.len(), dropped);
    drop(guard);
    let mut out = Vec::new();
    for rec in &recs[start..] {
        plog!("evt +{}ms {}", rec.t_ms, rec.msg);
        out.push(rec.msg.clone());
    }
    out
}

/// The real environment: the opened libGLESv2 and (with `--features gfx`) `plx_gfx`.
pub struct RealEnv {
    job: GfxJob,
    /// `plx_gfx`'s vertex buffer, read back from GL right after `init_gl` (it never rebinds it).
    vbo: u32,
    scratch_errors: u32,
}

impl RealEnv {
    pub fn new(job: GfxJob) -> RealEnv {
        RealEnv { job, vbo: 0, scratch_errors: 0 }
    }

    fn syms(&self) -> Option<&ProbeSyms> {
        self.job.syms.as_ref().ok()
    }
}

/// Bottom-right corner of `plx_gfx`'s fixed 1920x1080 canvas: the swatch's rect, in canvas pixels.
const SWATCH: (f32, f32, f32, f32) = (1568.0, 1004.0, 336.0, 64.0);

/// Run `body`, then ALWAYS `restore`, even when `body` panics: the panic then continues to the caller
/// (the probe's own `catch_unwind`, which names it in the report).
fn then_restore<R>(restore: impl FnOnce(), body: impl FnOnce() -> R) -> R {
    let result = catch_unwind(AssertUnwindSafe(body));
    restore();
    match result {
        Ok(v) => v,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Put the GL state the panel's own draw (`label_gl`) relies on back, after `plx_gfx` ran: no vertex buffer
/// bound (the panel's arrays are client-side) and no attribute left enabled, blending and the scissor test
/// off, dither on (all GL's defaults, which is what the panel was written against), texture unit 0 active.
/// Everything else the panel's draw sets for itself on every frame (program, texture, both attribute
/// pointers); the viewport is set by the swatch's own draw to the window's size, which is the panel's too.
fn restore_panel_gl(s: &ProbeSyms) {
    // SAFETY: the render thread's context is current.
    unsafe {
        (s.bind_buffer)(GL_ARRAY_BUFFER, 0);
        (s.disable_vertex_attrib_array)(0);
        (s.disable_vertex_attrib_array)(1);
        (s.disable)(GL_BLEND);
        (s.disable)(GL_SCISSOR_TEST);
        (s.enable)(GL_DITHER);
        (s.active_texture)(GL_TEXTURE0);
    }
}

impl Env for RealEnv {
    fn available(&self) -> bool {
        cfg!(feature = "gfx")
    }

    fn missing_gl(&self) -> Vec<String> {
        let mut m: Vec<String> = self.job.forwarders_missing.iter().map(|s| s.to_string()).collect();
        if let Err(names) = &self.job.syms {
            m.extend(names.iter().map(|s| s.to_string()));
        }
        m.sort();
        m.dedup();
        m
    }

    fn gl_strings(&mut self) -> GlStrings {
        let Some(s) = self.syms() else { return GlStrings::default() };
        let read = |name: u32| -> String {
            // SAFETY: a current context on this thread; the string is static or null.
            let p = unsafe { (s.get_string)(name) };
            if p.is_null() {
                String::new()
            } else {
                unsafe { std::ffi::CStr::from_ptr(p as *const c_char) }.to_string_lossy().into_owned()
            }
        };
        GlStrings { version: read(GL_VERSION), glsl: read(GL_SHADING_LANGUAGE_VERSION), extensions: read(GL_EXTENSIONS) }
    }

    fn shader_gl(&mut self) -> &mut dyn ShaderGl {
        // Only reached after `missing_gl` said the table is whole.
        let syms: &'static ProbeSyms = Box::leak(Box::new(*self.job.syms.as_ref().ok().expect("probe GL table")));
        Box::leak(Box::new(RealShaderGl(syms)))
    }

    fn drain_errors(&mut self) -> u32 {
        let Some(s) = self.syms() else { return 0 };
        let mut first = 0;
        for _ in 0..16 {
            // SAFETY: a current context on this thread.
            let e = unsafe { (s.get_error)() };
            if e == 0 {
                break;
            }
            if first == 0 {
                first = e;
            }
        }
        self.scratch_errors = first;
        first
    }

    fn init(&mut self, stage: Stage) {
        #[cfg(feature = "gfx")]
        {
            let syms = self.syms().copied();
            let mut vbo = self.vbo;
            // However the init ends, a panic included, the panel's GL state is put back: the panel's next
            // frame draws with client-side arrays and must find no buffer bound.
            then_restore(
                || {
                    if let Some(s) = &syms {
                        restore_panel_gl(s)
                    }
                },
                || match stage {
                    Stage::InitGl => {
                        plx_gfx::gfx::init_gl();
                        // `init_gl` leaves its vertex buffer bound and never rebinds it; keep its name so every
                        // later draw can put it back after the panel's client-side arrays used the binding.
                        if let Some(s) = &syms {
                            let mut b = 0i32;
                            // SAFETY: a current context on this thread; one int out.
                            unsafe { (s.get_integerv)(GL_ARRAY_BUFFER_BINDING, &mut b) };
                            vbo = b.max(0) as u32;
                            plog!("H7 gfx: plx_gfx's vertex buffer is {vbo}");
                        }
                    }
                    Stage::InitText => plx_gfx::text::init_text(),
                    Stage::InitImage => plx_gfx::gfx::init_image(),
                    Stage::InitBlur => plx_gfx::gfx::init_blur(),
                },
            );
            self.vbo = vbo;
        }
        #[cfg(not(feature = "gfx"))]
        let _ = stage;
    }

    fn draw_swatch(&mut self, width: i32, height: i32) -> f32 {
        let Some(s) = self.syms().copied() else { return 0.0 };
        // SAFETY (the GL calls below): the render thread's context is current.
        unsafe {
            // The panel drew just before this with client-side arrays, which need NO buffer bound and no
            // attribute enabled; `plx_gfx` assumes the opposite (its one buffer bound, attribute 0 read
            // from it at offset 0). Put its state back, and take it away again afterwards.
            (s.viewport)(0, 0, width, height);
            (s.bind_buffer)(GL_ARRAY_BUFFER, self.vbo);
            (s.enable_vertex_attrib_array)(0);
            (s.vertex_attrib_pointer)(0, 2, GL_FLOAT, 0, 0, std::ptr::null());
            (s.enable)(GL_BLEND);
            (s.blend_func_separate)(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA, GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
            (s.disable)(GL_DITHER);
            (s.disable)(GL_SCISSOR_TEST);
            (s.active_texture)(GL_TEXTURE0);
        }
        // The state is put back for the panel even when a draw panics (the panic then goes on to the
        // caller's own `catch_unwind`, which names it): a panel drawn with `plx_gfx`'s buffer still bound
        // would read its client-side arrays as offsets and vanish, which is the one thing this probe must
        // never cause.
        then_restore(
            || restore_panel_gl(&s),
            || {
                #[cfg(feature = "gfx")]
                {
                    use plx_gfx::gfx::Zoom;
                    // Forget which program `plx_gfx` believes is bound: the panel bound its own since.
                    plx_gfx::gfx::use_prog(0);
                    plx_gfx::text::begin_frame();
                    plx_gfx::gfx::straighten::frame_begin();
                    let (x, y, w, h) = SWATCH;
                    let fill = [0.85f32, 0.10, 0.80, 1.0];
                    let ink = [1.0f32, 1.0, 1.0, 1.0];
                    plx_gfx::gfx::draw_rect(x, y, w, h, 0.0, 12.0, fill.as_ptr(), fill.as_ptr(), 0.0, 1.0);
                    let px = plx_gfx::text::draw_text(c"plx_gfx text".as_ptr(), x + 18.0, y + 13.0, 30 as c_int, ink.as_ptr(), 0, 0, Zoom::NONE);
                    plx_gfx::gfx::use_prog(0);
                    px
                }
                #[cfg(not(feature = "gfx"))]
                {
                    let _ = SWATCH;
                    0.0
                }
            },
        )
    }

    fn font(&self) -> (String, bool) {
        #[cfg(feature = "gfx")]
        let expected = plx_gfx::text::app_font_path(false).display().to_string();
        #[cfg(not(feature = "gfx"))]
        let expected = String::new();
        (expected, FONT_REDIRECTS.load(std::sync::atomic::Ordering::Relaxed) > 0)
    }

    fn new_events(&mut self) -> Vec<String> {
        forward_events()
    }

    fn publish(&mut self, ok: bool, lines: [String; 2]) {
        panel::step_lines(Step::Gfx, ok, &lines);
    }

    fn unbound_calls(&self) -> (u32, Option<&'static str>) {
        #[cfg(feature = "gfx")]
        {
            crate::gl_shim::unbound_calls()
        }
        #[cfg(not(feature = "gfx"))]
        {
            (0, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    // --- the panel's GL state is put back whatever the draw does ------------------------------------

    #[test]
    fn the_panels_gl_state_is_restored_after_a_draw_and_after_one_that_panics() {
        let restored = std::cell::Cell::new(0u32);
        let got = then_restore(|| restored.set(restored.get() + 1), || 7.5f32);
        assert_eq!((got, restored.get()), (7.5, 1), "a normal draw: the value comes back and the state was restored once");

        let before = restored.get();
        let hushed = std::panic::catch_unwind(AssertUnwindSafe(|| {
            then_restore(|| restored.set(restored.get() + 1), || -> f32 { panic!("a draw blew up") })
        }));
        assert_eq!(restored.get(), before + 1, "a draw that panics still restores the state before the panic moves on");
        let payload = hushed.expect_err("the panic must reach the probe's own catch_unwind, not be swallowed here");
        assert_eq!(panic_text(payload.as_ref()), "a draw blew up", "the panic's own words survive, so the report can name it");
    }

    // --- a fake GL ---------------------------------------------------------------------------------

    /// Fails to compile a source containing `fail_on`, or fails to link a pair whose fragment source does.
    #[derive(Default)]
    struct FakeShaderGl {
        fail_compile_on: Option<&'static str>,
        fail_link_on: Option<&'static str>,
        next: u32,
        compiled: Vec<u32>,
        deleted: Vec<u32>,
        sources: std::collections::HashMap<u32, &'static str>,
    }

    impl ShaderGl for FakeShaderGl {
        fn compile(&mut self, _fragment: bool, src: &'static str) -> Result<u32, String> {
            if self.fail_compile_on.is_some_and(|m| src.contains(m)) {
                return Err("0:12: 'gl_LastFragColorARM' : undeclared identifier".to_string());
            }
            self.next += 1;
            self.compiled.push(self.next);
            self.sources.insert(self.next, src);
            Ok(self.next)
        }

        fn link(&mut self, vs: u32, fs: u32) -> Result<u32, String> {
            let _ = vs;
            if self.fail_link_on.is_some_and(|m| self.sources[&fs].contains(m)) {
                return Err("varying v_x not written by the vertex shader".to_string());
            }
            self.next += 1;
            Ok(self.next)
        }

        fn delete(&mut self, vs: u32, fs: u32, _program: Option<u32>) {
            self.deleted.extend([vs, fs].into_iter().filter(|n| *n != 0));
        }
    }

    // --- a fake environment ------------------------------------------------------------------------

    struct FakeEnv {
        available: bool,
        missing: Vec<String>,
        strings: GlStrings,
        shader: FakeShaderGl,
        errors: VecDeque<u32>,
        panic_in: Option<Stage>,
        draw_panics: bool,
        text_px: f32,
        log: Vec<String>,
        events: Vec<String>,
        unbound: (u32, Option<&'static str>),
        redirected: bool,
        published: Vec<(bool, [String; 2])>,
    }

    impl FakeEnv {
        fn healthy() -> FakeEnv {
            FakeEnv {
                available: true,
                missing: Vec::new(),
                strings: GlStrings {
                    version: "OpenGL ES 2.0 YaGL".into(),
                    glsl: "OpenGL ES GLSL ES 1.00".into(),
                    extensions: "GL_OES_texture_npot GL_EXT_shader_framebuffer_fetch".into(),
                },
                shader: FakeShaderGl::default(),
                errors: VecDeque::new(),
                panic_in: None,
                draw_panics: false,
                text_px: 187.0,
                log: Vec::new(),
                events: Vec::new(),
                unbound: (0, None),
                redirected: true,
                published: Vec::new(),
            }
        }
    }

    impl Env for FakeEnv {
        fn available(&self) -> bool {
            self.available
        }
        fn missing_gl(&self) -> Vec<String> {
            self.missing.clone()
        }
        fn gl_strings(&mut self) -> GlStrings {
            self.strings.clone()
        }
        fn shader_gl(&mut self) -> &mut dyn ShaderGl {
            &mut self.shader
        }
        fn drain_errors(&mut self) -> u32 {
            self.errors.pop_front().unwrap_or(0)
        }
        fn init(&mut self, stage: Stage) {
            self.log.push(format!("init {}", stage.name()));
            if self.panic_in == Some(stage) {
                panic!("{} blew up", stage.name());
            }
        }
        fn draw_swatch(&mut self, _w: i32, _h: i32) -> f32 {
            self.log.push("draw".to_string());
            if self.draw_panics {
                panic!("draw blew up");
            }
            self.text_px
        }
        fn font(&self) -> (String, bool) {
            ("/usr/bin/appfont.ttf".to_string(), self.redirected)
        }
        fn new_events(&mut self) -> Vec<String> {
            std::mem::take(&mut self.events)
        }
        fn unbound_calls(&self) -> (u32, Option<&'static str>) {
            self.unbound
        }
        fn publish(&mut self, ok: bool, lines: [String; 2]) {
            self.published.push((ok, lines));
        }
    }

    /// Drive a probe like the render loop does: one `frame` per iteration, `presented` after each swap.
    fn run(env: FakeEnv, iterations: u64) -> Probe<FakeEnv> {
        let _g = crate::testutil::log_serial(); // the probe logs; see `testutil::log_serial`
        let mut p = Probe::new(env);
        for n in 0..iterations {
            p.frame(n, 1920, 1080);
            p.presented();
        }
        p
    }

    // --- the shader table --------------------------------------------------------------------------

    #[test]
    fn every_pair_is_composed_from_real_shader_text_and_is_nul_terminated() {
        assert_eq!(PAIRS.len(), 14);
        for p in PAIRS {
            for (what, src) in [("vs", p.vs), ("fs", p.fs)] {
                assert!(src.ends_with('\0') && !src[..src.len() - 1].contains('\0'), "{} {what}: one trailing NUL", p.name);
                assert!(src.contains("void main"), "{} {what} has a main(): {:.60}", p.name, src);
            }
        }
        // The dithered fragment sources start with the dither prelude, the vertex variants with their define.
        assert!(FS_FIELD.0.contains("plx_dither"));
        assert!(VS_SRC_DITHERED.0.starts_with("#define PLX_DITHER_NC\n"));
        assert!(VS_SCRIM_LAYER.0.starts_with("#define PLX_SCRIM\n#define PLX_SCRIM_LAYER\n"));
    }

    /// The table must say what the renderer says: each constant is declared in `gfx.rs` / `text.rs` with
    /// the macro call recorded here. A shader the renderer re-composes (a new prefix, a different prelude)
    /// fails this instead of letting the preflight check something else than the process will compile.
    #[test]
    fn the_table_matches_the_declarations_in_gfx_rs_and_text_rs() {
        let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let haystack = squash(&format!(
            "{}\n{}",
            include_str!("../../../../rust-modules/gfx/src/gfx.rs"),
            include_str!("../../../../rust-modules/gfx/src/text.rs")
        ));
        for p in PAIRS {
            for (name, call) in [p.vs_decl, p.fs_decl] {
                let decl = format!("const {name}: &CStr = {call};");
                assert!(haystack.contains(&decl), "{}: the renderer no longer declares `{decl}`", p.name);
            }
        }
    }

    /// And the set itself: every `link_program(VS_*, FS_*)` the four init functions make (the call that
    /// exits on a rejection) is a pair here, and nothing here is a pair they do not make.
    #[test]
    fn the_pairs_are_exactly_the_required_link_program_calls_of_the_four_inits() {
        let mut in_source = std::collections::BTreeSet::new();
        for src in [
            include_str!("../../../../rust-modules/gfx/src/gfx.rs"),
            include_str!("../../../../rust-modules/gfx/src/text.rs"),
        ] {
            // The inits only: the optional programs go through `link_optional_program`, and the test
            // modules mention link_program in prose.
            // The match string begins at `link_program(VS_`, so `link_optional_program(` (a different name)
            // and `fn link_program(vs: ...` (no `VS_` right after the parenthesis) never match.
            for (i, _) in src.match_indices("link_program(VS_") {
                let tail: String = src[i + "link_program(".len()..].chars().take(80).collect();
                let tail = tail.replace(".as_ptr()", "");
                let mut args = tail.split(',').map(|a| a.split(')').next().unwrap_or("").trim().to_string());
                if let (Some(vs), Some(fs)) = (args.next(), args.next()) {
                    in_source.insert((vs, fs));
                }
            }
        }
        let ours: std::collections::BTreeSet<(String, String)> =
            PAIRS.iter().map(|p| (p.vs_decl.0.to_string(), p.fs_decl.0.to_string())).collect();
        // `gfx.rs` also has test-module calls with the same names; the sets must agree on the union.
        assert_eq!(ours, in_source, "required link_program pairs in the renderer vs this table");
    }

    // --- the preflight -----------------------------------------------------------------------------

    #[test]
    fn a_driver_that_accepts_everything_passes_every_pair_and_every_object_is_deleted() {
        let mut gl = FakeShaderGl::default();
        let results = preflight(&mut gl, PAIRS);
        assert_eq!(results.len(), 14);
        assert!(results.iter().all(|r| r.outcome.is_ok()));
        assert_eq!(gl.deleted.len(), gl.compiled.len(), "no shader object is left behind");
    }

    #[test]
    fn a_compile_failure_is_named_with_the_pair_and_the_drivers_words_and_does_not_stop_the_rest() {
        let mut gl = FakeShaderGl { fail_compile_on: Some("shadow"), ..Default::default() };
        // `fs_shadow.frag` is the only source that mentions the word in a comment or code; make sure the
        // marker is actually in it before relying on it.
        assert!(FS_SHADOW.0.contains("shadow"));
        let results = preflight(&mut gl, PAIRS);
        let failed: Vec<&PairResult> = results.iter().filter(|r| r.outcome.is_err()).collect();
        assert!(!failed.is_empty() && failed.iter().any(|r| r.name == "init_gl shadow"));
        let e = failed.iter().find(|r| r.name == "init_gl shadow").unwrap().outcome.clone().unwrap_err();
        assert!(e.starts_with("fragment: ") && e.contains("undeclared identifier"), "{e}");
        assert!(results.iter().any(|r| r.outcome.is_ok()), "the other pairs were still checked");
    }

    #[test]
    fn a_link_failure_is_reported_and_leaves_nothing_allocated() {
        let mut gl = FakeShaderGl { fail_link_on: Some("plx_dither"), ..Default::default() };
        let results = preflight(&mut gl, PAIRS);
        let first = results.iter().find(|r| r.outcome.is_err()).expect("a dithered pair fails to link");
        assert!(first.outcome.clone().unwrap_err().starts_with("link: "));
        assert_eq!(gl.deleted.len(), gl.compiled.len());
    }

    // --- the GL strings ----------------------------------------------------------------------------

    #[test]
    fn glsl_es_is_decided_by_the_prefix_like_the_renderers_own_test() {
        assert!(glsl_is_es("OpenGL ES GLSL ES 1.00"));
        assert!(glsl_is_es(""), "an unreadable string counts as ES, as in plx_gfx");
        assert!(!glsl_is_es("4.10"));
        assert!(!glsl_is_es("WebGL GLSL ES 1.0"), "a substring is not the prefix");
    }

    #[test]
    fn a_missing_extension_is_named_once_per_need_and_either_fetch_extension_meets_it() {
        assert_eq!(missing_extensions("GL_OES_texture_npot"), vec!["ARM|EXT_shader_framebuffer_fetch", "EXT_disjoint_timer_query"]);
        assert_eq!(missing_extensions("GL_ARM_shader_framebuffer_fetch GL_EXT_disjoint_timer_query"), Vec::<String>::new());
        assert_eq!(missing_extensions("GL_EXT_shader_framebuffer_fetch"), vec!["EXT_disjoint_timer_query"]);
        // Whole tokens only.
        assert_eq!(missing_extensions("GL_EXT_shader_framebuffer_fetch_non_coherent").len(), 2);
    }

    // --- the report and the panel lines -------------------------------------------------------------

    fn good_report() -> Report {
        Report {
            shaders: Some((14, 14, None)),
            stages: Stage::ALL.iter().map(|s| (s.name(), Ok(()))).collect(),
            err_init: Some(0),
            err_frame: Some(0),
            err_swap: Some(0),
            text_px: Some(187.0),
            rect_drawn: true,
            missing_ext: vec!["ARM|EXT_shader_framebuffer_fetch".into()],
            font_redirected: true,
            font_expected: "/usr/bin/appfont.ttf".into(),
            ..Report::default()
        }
    }

    #[test]
    fn a_good_report_is_green_two_lines_verdict_first_and_names_what_was_not_found() {
        let (ok, l) = good_report().lines();
        assert!(ok);
        assert_eq!(l[0], "OK: init_gl+text+image+blur ran; shaders 14/14 compile+link; rect drawn, text 187px (res/ font)");
        assert_eq!(l[1], "glGetError init/frame/swap 0/0/0; no GL ext: ARM|EXT_shader_framebuffer_fetch");
        assert!(l.iter().all(|s| s.chars().count() <= LINE_MAX), "{l:?}");
    }

    /// Both extensions missing (what an emulator will most likely say) is the longest good line 2: it must
    /// still show every name whole.
    #[test]
    fn both_missing_extensions_are_shown_whole_on_a_good_report() {
        let mut r = good_report();
        r.missing_ext = vec!["ARM|EXT_shader_framebuffer_fetch".into(), "EXT_disjoint_timer_query".into()];
        let (_, l) = r.lines();
        assert_eq!(l[1], "glGetError init/frame/swap 0/0/0; no GL ext: ARM|EXT_shader_framebuffer_fetch EXT_disjoint_timer_query");
        assert!(l[1].chars().count() <= LINE_MAX, "{}", l[1].chars().count());
    }

    #[test]
    fn a_gl_error_makes_the_step_not_ok_and_shows_its_code() {
        let mut r = good_report();
        r.err_frame = Some(0x0502);
        let (ok, l) = r.lines();
        assert!(!ok);
        assert!(l[0].starts_with("NOT OK: "), "{}", l[0]);
        assert!(l[1].starts_with("glGetError init/frame/swap 0/0x0502/0;"), "{}", l[1]);
    }

    #[test]
    fn text_that_drew_nothing_says_where_the_font_was_looked_for() {
        let mut r = good_report();
        r.text_px = Some(0.0);
        r.font_redirected = false;
        r.font_expected = "/usr/bin/appfont.ttf".into();
        let (ok, l) = r.lines();
        assert!(!ok);
        assert!(l[0].ends_with("rect drawn, TEXT DREW 0px"), "{}", l[0]);
        assert!(l[1].contains("no font face, looked in /usr/bin/appfont.ttf"), "{}", l[1]);
    }

    #[test]
    fn a_refusal_is_failed_first_and_the_second_line_keeps_the_shader_and_extension_facts() {
        let r = Report {
            refusal: Some("2 of 14 shader pairs do not compile+link (init would exit); first: init_gl shadow: fragment: 0:3: syntax".into()),
            shaders: Some((12, 14, Some("init_gl shadow: fragment: 0:3: syntax".into()))),
            missing_ext: vec!["EXT_disjoint_timer_query".into()],
            ..Report::default()
        };
        let (ok, l) = r.lines();
        assert!(!ok);
        assert!(l[0].starts_with("FAILED, plx_gfx not entered: 2 of 14 shader pairs"), "{}", l[0]);
        assert!(l[1].starts_with("shaders 12/14 compile+link, first failure init_gl shadow"), "{}", l[1]);
        assert!(l.iter().all(|s| s.chars().count() <= LINE_MAX), "{l:?}");
    }

    #[test]
    fn a_panicked_init_is_named_and_the_lines_stay_inside_the_cap() {
        let mut r = good_report();
        r.stages[1] = ("init_text", Err("called `Option::unwrap()` on a `None` value".into()));
        r.failure_events = 3;
        r.first_failure_event = Some("text prog link failed with a very long explanation of why".into());
        let (ok, l) = r.lines();
        assert!(!ok);
        assert!(l[0].contains("init_text PANICKED"), "{}", l[0]);
        assert!(l[1].contains("3 failure line(s)"), "{}", l[1]);
        assert!(l.iter().all(|s| s.chars().count() <= LINE_MAX), "{l:?}");
    }

    #[test]
    fn the_panel_text_of_a_good_run_and_a_refusal_fits_the_two_line_budget() {
        for r in [good_report(), Report { refusal: Some("x".repeat(400)), ..Report::default() }] {
            let (_, l) = r.lines();
            assert!(l.iter().all(|s| s.chars().count() <= LINE_MAX));
        }
    }

    #[test]
    fn failure_lines_are_recognised_by_the_renderers_own_words() {
        for l in ["shader compile FAILED: 0:1: bad", "base prog link FAILED — exiting", "text prog link failed", "TTF_Init failed"] {
            assert!(looks_like_failure(l), "{l}");
        }
        assert!(!looks_like_failure("init_text ok=1"));
    }

    // --- the ring cursor ---------------------------------------------------------------------------

    #[test]
    fn the_ring_cursor_hands_out_each_line_once_and_survives_eviction() {
        let mut c = RingCursor::default();
        assert_eq!(c.advance(3, 0), 0, "first read: everything");
        assert_eq!(c.advance(3, 0), 3, "nothing new");
        assert_eq!(c.advance(5, 0), 3, "two new lines at the end");
        // Four lines were evicted from the front and two more arrived: the ring now holds lines 4..11.
        assert_eq!(c.advance(7, 4), 5 + 0 - 4, "one consumed line is still in the ring, the rest is new");
        // Everything evicted before we could read it: start at the beginning of what is left.
        assert_eq!(c.advance(7, 100), 0);
    }

    // --- the state machine -------------------------------------------------------------------------

    /// Phases advance one render-loop frame at a time, so the panel shows how far the probe got.
    #[test]
    fn the_probe_waits_for_the_start_frame_then_steps_one_phase_per_frame() {
        let _g = crate::testutil::log_serial();
        let mut p = Probe::new(FakeEnv::healthy());
        p.frame(0, 1920, 1080);
        p.frame(1, 1920, 1080);
        assert_eq!(p.phase(), Phase::Idle, "the panel's own first frames come first");
        let mut seen = Vec::new();
        for n in START_FRAME..START_FRAME + 4 {
            p.frame(n, 1920, 1080);
            seen.push(p.phase());
            p.presented();
        }
        assert_eq!(seen, vec![Phase::Preflight, Phase::Init, Phase::FirstDraw, Phase::AwaitSwap]);
        assert_eq!(p.phase(), Phase::Live, "the presented swap closed the report");
    }

    #[test]
    fn a_healthy_run_enters_the_four_inits_in_the_apps_order_draws_every_frame_and_ends_ok() {
        let p = run(FakeEnv::healthy(), START_FRAME + 10);
        assert_eq!(p.env.log[..4], ["init init_gl", "init init_text", "init init_image", "init init_blur"]);
        assert!(p.env.log.iter().filter(|l| *l == "draw").count() >= 6, "the swatch is redrawn on every frame");
        assert!(p.report.is_ok(), "{:?}", p.report);
        assert_eq!(p.phase(), Phase::Live);
    }

    #[test]
    fn a_shader_the_driver_rejects_means_plx_gfx_is_never_entered() {
        let mut env = FakeEnv::healthy();
        env.shader.fail_compile_on = Some("fs_flat");
        assert!(FS_FLAT.0.contains("fs_flat") || FS_FLAT.0.contains("u_col"), "pick a marker the source really has");
        env.shader.fail_compile_on = Some("u_col");
        let p = run(env, START_FRAME + 6);
        assert!(p.env.log.is_empty(), "no init, no draw: the renderer exits the process on a rejected shader");
        assert_eq!(p.phase(), Phase::Done);
        let (ok, l) = p.report.lines();
        assert!(!ok && l[0].starts_with("FAILED, plx_gfx not entered: "), "{l:?}");
        assert!(p.report.shaders.as_ref().is_some_and(|(ok, total, first)| ok < total && first.is_some()));
    }

    #[test]
    fn a_missing_gl_entry_or_a_desktop_glsl_stops_before_any_gl_call_is_made() {
        let mut env = FakeEnv::healthy();
        env.missing = vec!["glBlendFuncSeparate".into()];
        let p = run(env, START_FRAME + 3);
        assert!(p.env.log.is_empty());
        assert!(p.report.refusal.as_deref().unwrap().contains("glBlendFuncSeparate"));

        let mut env = FakeEnv::healthy();
        env.strings.glsl = "4.10".into();
        let p = run(env, START_FRAME + 3);
        assert!(p.report.refusal.as_deref().unwrap().contains("is not OpenGL ES"));
        assert!(p.env.log.is_empty());
    }

    #[test]
    fn a_build_without_plx_gfx_says_so_and_does_nothing_else() {
        let mut env = FakeEnv::healthy();
        env.available = false;
        let p = run(env, START_FRAME + 3);
        assert!(p.report.refusal.as_deref().unwrap().contains("--no-gfx"));
        assert!(p.env.log.is_empty());
        assert_eq!(p.phase(), Phase::Done);
    }

    #[test]
    fn a_panicking_init_is_caught_recorded_and_the_swatch_is_not_drawn() {
        let mut env = FakeEnv::healthy();
        env.panic_in = Some(Stage::InitImage);
        let p = run(env, START_FRAME + 8);
        assert_eq!(p.report.stages.len(), 4, "the other inits still ran after the panic");
        assert!(p.report.stages[2].1.is_err());
        assert!(!p.env.log.iter().any(|l| l == "draw"));
        let (ok, l) = p.report.lines();
        assert!(!ok && l[0].contains("init_image PANICKED"), "{l:?}");
        assert_eq!(p.phase(), Phase::Done);
    }

    #[test]
    fn a_panicking_draw_stops_the_swatch_and_is_reported_not_propagated() {
        let mut env = FakeEnv::healthy();
        env.draw_panics = true;
        let p = run(env, START_FRAME + 8);
        assert_eq!(p.phase(), Phase::Done);
        assert!(p.report.late_fault.as_deref().unwrap().contains("draw PANICKED"));
        assert!(!p.report.is_ok());
    }

    #[test]
    fn gl_errors_are_attributed_to_the_stage_that_saw_them() {
        let mut env = FakeEnv::healthy();
        // drain order: preflight's drain, after init, after the draw, after the swap
        env.errors = VecDeque::from([0, 0x0501, 0x0502, 0x0506]);
        let p = run(env, START_FRAME + 6);
        assert_eq!((p.report.err_init, p.report.err_frame, p.report.err_swap), (Some(0x0501), Some(0x0502), Some(0x0506)));
    }

    #[test]
    fn unbound_gl_calls_after_the_swatch_is_up_are_reported() {
        let mut env = FakeEnv::healthy();
        env.unbound = (2, Some("glFooBar"));
        let p = run(env, START_FRAME + 8);
        assert!(p.report.late_fault.as_deref().unwrap().contains("glFooBar"));
    }

    #[test]
    fn failure_lines_from_the_renderers_log_reach_the_report() {
        let mut env = FakeEnv::healthy();
        env.events = vec!["init_text ok=1".into(), "text fade prog link failed".into()];
        let p = run(env, START_FRAME + 6);
        assert_eq!(p.report.failure_events, 1);
        assert_eq!(p.report.first_failure_event.as_deref(), Some("text fade prog link failed"));
    }

    #[test]
    fn every_phase_publishes_two_lines_inside_the_cap_and_the_last_one_is_the_verdict() {
        let p = run(FakeEnv::healthy(), START_FRAME + 6);
        assert!(p.env.published.len() >= 4, "a provisional line per phase, then the report");
        for (_, l) in &p.env.published {
            assert!(l.iter().all(|s| !s.is_empty() && s.chars().count() <= LINE_MAX), "{l:?}");
        }
        let (ok, last) = p.env.published.last().unwrap();
        assert!(*ok);
        assert!(last[0].starts_with("OK: init_gl+text+image+blur ran;"), "{last:?}");
        assert!(last[1].starts_with("glGetError init/frame/swap"), "{last:?}");
        // The provisional lines come first (not a verdict yet).
        assert!(p.env.published[0].1[0].starts_with("preflight:"), "{:?}", p.env.published[0]);
    }
}
