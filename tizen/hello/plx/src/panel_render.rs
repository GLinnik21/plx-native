//! The panel's pixels: SDL_ttf rasterises the text model's sections ([`crate::panel`]) into ONE opaque
//! RGBA image, and [`Drawer`] keeps that image on screen as a GLES2 texture (`label_gl`).
//!
//! Everything here runs on the RENDER thread: the font is opened and dropped there (no SDL object
//! crosses a thread), the main thread only mutates the text model. The image is re-rasterised and
//! re-uploaded only when the model's version moved ([`panel::Tracker`]); a still panel costs one
//! textured quad per frame. A failure here is never fatal: the screen stays the step-1 colour test.
//!
//! The image is authored at a fixed 1080-line reference ([`PANEL_W`] x [`PANEL_H`], text at
//! [`FONT_PX`] px, which is at least the 28 px a phone photo of the TV needs) and drawn centred by
//! `label_gl`, scaled by `screen_height / 1080`.
//!
//! The rasteriser is behind `--cfg plx_sdl` like the rest of the SDL code; a library built without SDL
//! has a [`Renderer`] that refuses to open, so the panel is simply off.

use crate::h5::{self, Label};
use crate::label_gl::{GlLabel, LabelSyms};
use crate::log::plog;
use crate::panel::{self, Section, Tracker};
use std::time::Instant;

/// The image's size in pixels at the 1080-line reference: the screen minus a 24 px frame of the
/// step-1 colour on each side (that frame is what still changes colour on a key press).
pub const PANEL_W: i32 = 1872;
pub const PANEL_H: i32 = 1048;
/// Body text size. SDL_ttf's point size is the pixel em at 72 dpi.
pub const FONT_PX: i32 = 28;
/// Distance between two lines of one wrapped section, in pixels. The font's own is 34 px at [`FONT_PX`] (Inter's
/// 1.21 em); 31 px (1.11 em) leaves the glyphs untouched, with their ascenders and descenders still clear of
/// the next row, and returns about 3 px for every wrapped row: with the margins ([`PAD`], `GAP`) that is what
/// keeps the panel that step 8's four-line result and the shader bisect line make fit the plate.
#[cfg(plx_sdl)]
const LINE_PITCH: i32 = 31;
/// Inner padding of the plate and the space between two sections, in pixels. (Were 16 and 4 until step 8's
/// result grew to four lines, with the first shader error and the bisect summary: the vertical room is the
/// panel's scarce resource and the text size is fixed at [`FONT_PX`], so the margins gave way, not the text.)
const PAD: i32 = 12;
#[cfg(plx_sdl)]
const GAP: i32 = 2;
/// The lowest pixel row (exclusive) text may reach: the plate's height less the bottom padding. THIS is the
/// bound a panel must fit under, not [`PANEL_H`]: a section ending below it touches the plate's edge and
/// counts as overflowed ([`Rendered::overflowed`]), though it is still inside the image.
pub const CONTENT_LIMIT: i32 = PANEL_H - PAD;
/// The dark plate (also what the host test checks the corner against).
#[cfg(plx_sdl)]
pub const PLATE_RGB: [u8; 3] = [20, 20, 26];

/// ASCII stand-ins for characters the font may lack, tried before a `?`. Only used for a character
/// the font does NOT provide, so a font that has the glyph shows the real one.
#[cfg_attr(not(plx_sdl), allow(dead_code))]
const FALLBACKS: &[(char, &str)] = &[
    ('\u{23EA}', "<<"),
    ('\u{23E9}', ">>"),
    ('\u{2212}', "-"),
    ('\u{2014}', "-"),
    ('\u{2013}', "-"),
    ('\u{2713}', "ok"),
];

/// The text as it can be drawn: every character `has` provides is kept, the rest is replaced from
/// [`FALLBACKS`] or by `?`. Replaced characters are appended to `replaced` (once each). Never touches
/// `\n`, and ASCII is always kept (the font has it; a `has` that disagrees must not eat a letter).
#[cfg_attr(not(plx_sdl), allow(dead_code))]
pub fn displayable(has: impl Fn(char) -> bool, text: &str, replaced: &mut Vec<char>) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\n' || c.is_ascii() || has(c) {
            out.push(c);
            continue;
        }
        if !replaced.contains(&c) {
            replaced.push(c);
        }
        match FALLBACKS.iter().find(|(f, _)| *f == c) {
            Some((_, s)) => out.push_str(s),
            None => out.push('?'),
        }
    }
    out
}

/// One rasterised panel.
pub struct Rendered {
    /// Opaque RGBA, `PANEL_W x PANEL_H`.
    pub image: Label,
    /// Some section ended below [`CONTENT_LIMIT`] (the plate's height less its bottom padding): its tail is
    /// clipped by the plate's edge or touches it.
    pub overflowed: bool,
    /// Characters the font lacked and `displayable` replaced.
    pub replaced: Vec<char>,
    /// The pixel row just below the last section's text, WITHOUT the gap that follows a section: the number
    /// to compare with [`CONTENT_LIMIT`] (how full the plate is). It used to include that trailing gap, which
    /// made a panel that overflowed by 4 px read as "1040 of 1048" against the plate's height.
    pub used_height: i32,
}

#[cfg(plx_sdl)]
mod imp {
    use super::*;
    use crate::panel::Tone;
    use crate::sdl::{self, Color, Font, Surface};

    fn color(t: Tone) -> Color {
        let (r, g, b) = match t {
            Tone::Title => (255, 214, 10),
            Tone::Hint => (120, 220, 255),
            Tone::Fact => (232, 232, 236),
            Tone::Ok => (120, 230, 140),
            Tone::Warn => (255, 150, 80),
            Tone::Key => (190, 205, 255),
        };
        Color { r, g, b, a: 255 }
    }

    /// An open font; rasterises sections. Owned by the render thread.
    pub struct Renderer {
        font: Font,
    }

    impl Renderer {
        pub fn open(font_path: &str) -> Result<Renderer, String> {
            Font::open(font_path, FONT_PX).map(|mut font| {
                font.set_line_skip(LINE_PITCH);
                Renderer { font }
            })
        }

        /// The font the package carries: `<res dir>/appfont.ttf`.
        pub fn open_from_res(res_dir: Option<&str>) -> Result<Renderer, String> {
            let res = res_dir
                .map(|d| d.trim_end_matches('/'))
                .filter(|d| !d.is_empty())
                .ok_or_else(|| "the host gave no resource directory".to_string())?;
            Renderer::open(&format!("{res}/{}", h5::FONT_FILE))
        }

        /// Whether the font has `c` (the tests probe which fallbacks are in play).
        #[cfg(test)]
        pub fn has_glyph(&self, c: char) -> bool {
            self.font.has_glyph(c)
        }

        /// Draw the sections top to bottom on the dark plate, each wrapped to the plate's width.
        pub fn render(&self, sections: &[Section]) -> Result<Rendered, String> {
            let mut plate = Surface::new_rgb(PANEL_W, PANEL_H, sdl::PIXELFORMAT_ARGB8888)?;
            plate.fill(PLATE_RGB[0], PLATE_RGB[1], PLATE_RGB[2], 255)?;
            let wrap = (PANEL_W - 2 * PAD) as u32;
            let (mut y, mut overflowed, mut replaced) = (PAD, false, Vec::new());
            let mut bottom = PAD;
            for s in sections.iter().filter(|s| !s.text.trim().is_empty()) {
                let text = displayable(|c| self.font.has_glyph(c), &s.text, &mut replaced);
                let mut surf = self.font.render_blended_wrapped(&text, color(s.tone), wrap)?;
                surf.set_blend(sdl::BLENDMODE_BLEND)?;
                bottom = y + surf.height();
                if bottom > CONTENT_LIMIT {
                    overflowed = true;
                }
                // SDL clips a blit at the destination's edge, so an overflowing tail is cut, not a fault.
                surf.blit_onto(&mut plate, PAD, y)?;
                y = bottom + GAP;
            }
            let rgba = plate.to_rgba_opaque()?;
            Ok(Rendered { image: Label { rgba, width: PANEL_W, height: PANEL_H }, overflowed, replaced, used_height: bottom })
        }
    }
}

#[cfg(not(plx_sdl))]
mod imp {
    use super::*;

    /// Without static SDL there is no rasteriser: the panel is off and the screen is the colour test.
    pub struct Renderer;

    impl Renderer {
        pub fn open_from_res(_res_dir: Option<&str>) -> Result<Renderer, String> {
            Err("built without SDL, no text rasteriser".to_string())
        }

        pub fn render(&self, _sections: &[Section]) -> Result<Rendered, String> {
            Err("built without SDL".to_string())
        }
    }
}

pub use imp::Renderer;

/// What the render thread needs to start the panel: the GL entry points and where the font is.
pub struct PanelJob {
    pub syms: LabelSyms,
    pub res_dir: Option<String>,
}

/// The render thread's panel: GL program + texture, the rasteriser, and the version it last drew.
pub struct Drawer {
    gl: GlLabel,
    renderer: Renderer,
    tracker: Tracker,
    renders: u32,
    /// Characters already reported as replaced (so a missing glyph is logged once).
    reported: Vec<char>,
    /// A failed render with no image to fall back on: the panel is off for good.
    dead: bool,
}

impl Drawer {
    /// Render thread, context current. `None` (reported through `h5`) leaves the colour test alone.
    pub fn start(job: PanelJob) -> Option<Drawer> {
        let gl = match GlLabel::new(job.syms) {
            Ok(g) => g,
            Err(e) => {
                h5::gl_done(Err(e));
                return None;
            }
        };
        let renderer = match Renderer::open_from_res(job.res_dir.as_deref()) {
            Ok(r) => r,
            Err(e) => {
                h5::fail("panel-font", &e);
                h5::incomplete("the panel font could not be opened on the render thread");
                gl.destroy();
                return None;
            }
        };
        plog!("panel: renderer ready (font {FONT_PX}px, image {PANEL_W}x{PANEL_H} at the 1080-line reference)");
        Some(Drawer { gl, renderer, tracker: Tracker::default(), renders: 0, reported: Vec::new(), dead: false })
    }

    /// The panel's text moved since the last render (and the panel is alive).
    pub fn stale(&self) -> bool {
        !self.dead && self.tracker.stale(panel::version())
    }

    /// Re-rasterise and re-upload if the text changed. A failure keeps the previous image (if any).
    pub fn refresh(&mut self) {
        if !self.stale() {
            return;
        }
        let (version, sections) = panel::snapshot();
        let t0 = Instant::now();
        let outcome = self.renderer.render(&sections).and_then(|r| {
            let raster_ms = t0.elapsed().as_secs_f64() * 1000.0;
            self.gl.set_image(&r.image).map(|()| (r, raster_ms))
        });
        self.tracker.rendered(version);
        match outcome {
            Ok((r, raster_ms)) => {
                self.renders += 1;
                plog!(
                    "panel: render #{} (content v{version}) {} sections, rasterised in {raster_ms:.1} ms, uploaded; text reaches row {} of {} usable{}",
                    self.renders,
                    sections.len(),
                    r.used_height,
                    CONTENT_LIMIT,
                    if r.overflowed { " (OVERFLOW: the last section is clipped)" } else { "" }
                );
                let new: Vec<char> = r.replaced.iter().copied().filter(|c| !self.reported.contains(c)).collect();
                if !new.is_empty() {
                    plog!("panel: the font lacks {new:?}; ASCII stand-ins are drawn (this is not an H5 failure)");
                    self.reported.extend(new);
                }
            }
            Err(e) => {
                plog!("panel: render of content v{version} FAILED: {e}");
                if !self.gl.has_image() {
                    self.dead = true;
                    h5::fail("panel-render", &e);
                    h5::incomplete("the first panel render failed; the screen stays the colour test");
                }
            }
        }
    }

    /// Draw the current image. `Some(glGetError)` when a frame's quad was drawn, `None` when there is
    /// no image (off, or not yet rendered).
    pub fn draw(&self, screen_w: i32, screen_h: i32) -> Option<u32> {
        self.gl.has_image().then(|| self.gl.draw(screen_w, screen_h))
    }

    pub fn destroy(&self) {
        self.gl.destroy();
    }
}

#[cfg(all(test, plx_sdl))]
mod tests {
    use super::*;
    use crate::panel::{parse_host_facts, NativeFacts, Panel, Step, Tone};
    use crate::sdl;

    fn font_path() -> String {
        format!("{}/../../../pkg/{}", env!("CARGO_MANIFEST_DIR"), h5::FONT_FILE)
    }

    /// SDL_ttf up, a renderer open; the guard serialises the tests that touch process-global SDL state.
    fn with_renderer<R>(f: impl FnOnce(&Renderer) -> R) -> R {
        let _g = h5::test_guard();
        sdl::ttf_init().unwrap();
        let r = Renderer::open(&font_path()).expect("the shipped font opens");
        let out = f(&r);
        drop(r);
        sdl::ttf_quit();
        out
    }

    /// Which step-8 result a fixture carries.
    #[derive(Clone, Copy, PartialEq)]
    enum Gfx {
        /// A device where everything works and the GL lists neither wanted extension: the bisect and the link line say every
        /// variant compiles and links (one short line each), 60 frames drawn with `plx_gfx`.
        Ok,
        /// What the NEXT emulator run is expected to show if the split of the corner uniforms is what matters: all 14
        /// pairs compile, `plx_gfx` is entered and draws, both extensions are missing, and the bisect shows
        /// the original (a0-) failing, everything split compiling, and the link line as the hypothesis predicts.
        Emulator,
        /// The longest refusal the probe can write from realistic strings: a first error that wraps onto a
        /// second row, more log lines behind it, every rung failing, and the driver's copy of the source
        /// differing on all of them (the longest bisect line), and no variant linking (the longest link line).
        Worst,
        /// The longest result of a run that DID enter `plx_gfx` and went wrong: a panicked init with a long
        /// message, GL errors, no font, a late fault, `plx_gfx`'s own failure lines, both extensions missing
        /// and every bisect rung and link variant failing.
        WorstEntered,
    }

    /// The ladder's ids, in the order the bisect runs them.
    const LADDER: [&str; 15] = ["a0", "a", "b", "c", "d", "e", "f", "g", "h", "i", "k", "l", "m", "n", "o"];

    /// A bisect result where exactly the variants named in `pass` compiled.
    fn bisect(pass: &[&str], copy_differs: bool) -> crate::shader_bisect::BisectReport {
        use crate::gfx_probe::{Readback, Trial};
        use crate::shader_bisect::{BisectReport, Inspection, Outcome};
        let outcomes = LADDER
            .iter()
            .map(|id| Outcome {
                id: *id,
                trial: Trial {
                    compiled: pass.contains(id),
                    readback: copy_differs.then(|| Readback { returned: 6139, same: false, first_diff: Some(6139) }),
                    ..Trial::default()
                },
            })
            .collect();
        BisectReport { outcomes, bytes: Inspection::default() }
    }

    /// The ladder with only `fail` failing.
    fn bisect_but(fail: &[&str]) -> crate::shader_bisect::BisectReport {
        let pass: Vec<&str> = LADDER.iter().copied().filter(|id| !fail.contains(id)).collect();
        bisect(&pass, false)
    }

    /// A link result with the given variants ending as stated.
    fn links(ends: &[(&'static str, crate::gfx_probe::LinkVerdict)]) -> crate::shader_bisect::LinkReport {
        use crate::gfx_probe::{LinkTrial, LinkVerdict as V, Status};
        use crate::shader_bisect::{LinkOutcome, LinkReport};
        let st = |ok: bool| Status { ok, log: String::new() };
        let outcomes = ends
            .iter()
            .map(|(id, v)| LinkOutcome {
                id: *id,
                trial: match v {
                    V::Linked => LinkTrial { vs: st(true), fs: st(true), link: Some(st(true)) },
                    V::VertexFailed => LinkTrial { vs: st(false), fs: st(true), link: None },
                    V::FragmentFailed => LinkTrial { vs: st(true), fs: st(false), link: None },
                    V::BothFailed => LinkTrial { vs: st(false), fs: st(false), link: None },
                    V::LinkFailed => LinkTrial { vs: st(true), fs: st(true), link: Some(st(false)) },
                },
            })
            .collect();
        LinkReport { outcomes }
    }

    const LINK_IDS: [&str; 8] = ["L1", "L2", "L3", "L4", "L5", "L5v", "i0", "i2"];

    /// Every link variant ending the same way.
    fn links_all(v: crate::gfx_probe::LinkVerdict) -> crate::shader_bisect::LinkReport {
        links(&LINK_IDS.map(|id| (id, v)))
    }

    /// Steps 8 and 9 as the probes really word them (`Report::lines`, `NetReport::lines`). `net_typical`: a
    /// mock that answers; otherwise the longest failure step 9 can write (a long curl version string and
    /// host name).
    fn add_probe_steps(p: &Panel, gfx: Gfx, net_typical: bool) {
        use crate::gfx_probe::{LinkVerdict as V, Report, ShaderCheck, ShaderFailure, Where};
        use crate::net_probe::{CurlFacts, GetResult, MockHost, NetReport};
        let ext = vec!["ARM|EXT_shader_framebuffer_fetch".to_string(), "EXT_disjoint_timer_query".to_string()];
        let gfx = match gfx {
            Gfx::Ok => Report {
                shaders: Some(ShaderCheck { passed: 14, total: 14, first_failure: None }),
                bisect: Some(bisect(&LADDER, false)),
                links: Some(links_all(crate::gfx_probe::LinkVerdict::Linked)),
                stages: ["init_gl", "init_text", "init_image", "init_blur"].iter().map(|n| (*n, Ok(()))).collect(),
                err_init: Some(0),
                err_frame: Some(0),
                err_swap: Some(0),
                text_px: Some(187.0),
                rect_drawn: true,
                frames_drawn: crate::gfx_probe::STEADY_FRAMES,
                missing_ext: ext,
                font_redirected: true,
                ..Report::default()
            },
            Gfx::Emulator => Report {
                shaders: Some(ShaderCheck { passed: 14, total: 14, first_failure: None }),
                bisect: Some(bisect_but(&["a0"])),
                links: Some(links(&[("L1", V::Linked), ("L2", V::Linked), ("L3", V::VertexFailed), ("L4", V::LinkFailed), ("L5", V::LinkFailed), ("L5v", V::Linked), ("i0", V::VertexFailed), ("i2", V::Linked)])),
                stages: ["init_gl", "init_text", "init_image", "init_blur"].iter().map(|n| (*n, Ok(()))).collect(),
                err_init: Some(0),
                err_frame: Some(0),
                err_swap: Some(0),
                text_px: Some(187.0),
                rect_drawn: true,
                frames_drawn: crate::gfx_probe::STEADY_FRAMES,
                missing_ext: ext,
                font_redirected: true,
                ..Report::default()
            },
            Gfx::WorstEntered => Report {
                shaders: Some(ShaderCheck { passed: 14, total: 14, first_failure: None }),
                bisect: Some(bisect(&[], false)),
                links: Some(links_all(crate::gfx_probe::LinkVerdict::BothFailed)),
                stages: ["init_gl", "init_text", "init_image", "init_blur"].iter().map(|n| (*n, Ok(()))).collect(),
                err_init: Some(0x0502),
                err_frame: Some(0x0506),
                err_swap: Some(0x0501),
                text_px: Some(0.0),
                rect_drawn: true,
                frames_drawn: 7,
                missing_ext: ext,
                font_redirected: false,
                font_expected: "/opt/usr/globalapps/plxnhello0/bin/appfont.ttf".into(),
                failure_events: 12,
                first_failure_event: Some("text prog link failed: the vertex shader and the fragment shader do not agree on a varying named v_uv".into()),
                late_fault: Some("3 unbound GL call(s), first glGetFramebufferAttachmentParameteriv".into()),
                ..Report::default()
            },
            Gfx::Worst => Report {
                refusal: Some("3 of 14 shader pairs do not compile+link (init would exit)".into()),
                shaders: Some(ShaderCheck {
                    passed: 11,
                    total: 14,
                    first_failure: Some(ShaderFailure {
                        pair: "init_gl ambient plain",
                        at: Where::Fragment,
                        first_line: "0:37(9): error: syntax error, unexpected NEW_IDENTIFIER, expecting ',' or ';' or '=' or '[' or '{' or 'in' or 'out' or 'uniform' or 'varying'".into(),
                        more_lines: 2,
                    }),
                }),
                bisect: Some(bisect(&[], true)),
                links: Some(links_all(crate::gfx_probe::LinkVerdict::BothFailed)),
                missing_ext: ext,
                ..Report::default()
            },
        };
        let (ok, l) = gfx.lines();
        p.step_lines(Step::Gfx, ok, &l);
        let version = "libcurl/8.5.0 OpenSSL/3.0.13 zlib/1.3 brotli/1.1.0 zstd/1.5.5 libidn2/2.3.7 libpsl/0.21.2 nghttp2/1.59.0".to_string();
        let net = if net_typical {
            NetReport {
                curl: CurlFacts { loaded: Ok("libcurl.so.4"), init_ok: true, version: Some("libcurl/7.53.1 OpenSSL/1.0.2k zlib/1.2.8 libidn/1.33".into()) },
                mock: MockHost::Set("10.0.2.2:32499".into()),
                get: Some(GetResult::Reply { status: 200, body: br#"{"MediaContainer":{"machineIdentifier":"6f9c0a1e2b3d4c5e6f7a8b9c0d1e2f3a4b5c6d7e"}}"#.to_vec() }),
                pending: false,
            }
        } else {
            NetReport {
                curl: CurlFacts { loaded: Ok("libcurl.so.4"), init_ok: true, version: Some(version) },
                mock: MockHost::Set("a-very-long-host-name-for-the-tester-pc.example.com:32499".into()),
                get: Some(GetResult::Failed { rc: Some(56), timed_out: false }),
                pending: false,
            }
        };
        let (ok, l) = net.lines();
        p.step_lines(Step::Net, ok, &l);
    }

    fn typical_panel() -> Panel {
        let p = Panel::new();
        p.set_host_facts(parse_host_facts(
            "platform.version=6.0\nnative.api=6.0\nplatform.name=Tizen\nmodel_name=QE55Q60AAUXXU\nmanufacturer=Samsung\n\
             build.string=T-KTSUAKUC-1234.5\ncpu.armv7=True\ncpu.aarch64=False\ncpu.x86=False\ncpu.x86_64=False\n\
             runtime=.NET 8.0.8\nos=Linux 4.1.10-tizen #1 SMP PREEMPT Tue Mar 1 12:00:00 KST 2022\nosarch=Arm\nprocarch=Arm\nintptr=4\n",
        ));
        p.set_native(NativeFacts {
            arch: "arm".into(),
            ptr_bits: 32,
            machine: "armv7l".into(),
            glibc: "2.30".into(),
            exe: "/usr/bin/dotnet".into(),
        });
        p.step(Step::So, true, "libplx.so loaded, plx_tizen_create -> 0 (OK)");
        p.step(Step::Libs, true, "dlopen ok: ecore_wl2 ecore ecore_input wayland-egl EGL GLESv2");
        p.step(Step::Window, true, "Ecore_Wl2 window 1920x1080 shown, key handlers 4 registered");
        p.step(Step::Egl, true, "EGL 1.4 vendor \"ARM\" (1.4 Midgard-r28p0) context + window surface created");
        p.step(Step::Gl, true, "vendor \"ARM\" renderer \"Mali-T860\" version \"OpenGL ES 3.2\"");
        p.step(Step::Sdl, true, "H5 static SDL2 + SDL2_ttf: OK");
        p.step(Step::Sample, true, crate::panel::SAMPLE_TEXT);
        add_probe_steps(&p, Gfx::Ok, true);
        p.lifecycle(0.4, "focus IN");
        p.lifecycle(12.3, "pause");
        p.lifecycle(15.1, "resume, paused 2.8s");
        // A sweep of the remote: every press is a DOWN and an UP.
        for (n, c) in [
            ("Up", 111u32), ("Down", 116), ("Left", 113), ("Right", 114), ("Return", 36), ("XF86Back", 166), ("XF86AudioPlay", 207),
            ("XF86AudioPause", 208), ("XF86AudioStop", 209), ("XF86AudioRewind", 210), ("XF86AudioNext", 171), ("XF86Red", 190),
            ("XF86Green", 191), ("XF86Yellow", 192), ("XF86Blue", 193), ("1", 10), ("2", 11), ("3", 12), ("4", 13), ("5", 14),
            ("6", 15), ("7", 16), ("8", 17), ("9", 18), ("0", 19), ("XF86ChannelUp", 220), ("XF86ChannelDown", 221),
        ] {
            p.key_event(true, n, c);
            p.key_event(false, n, c);
        }
        p
    }

    /// Long but plausible: the longest strings a real TV is likely to report (a Mali GL_VERSION with a
    /// build tail, a model with a region suffix, a full kernel banner, a deep exe path), words and digits
    /// rather than a wall of capitals. Must still fit on the plate.
    fn long_panel() -> Panel {
        let p = typical_panel();
        p.set_host_facts(parse_host_facts(
            "model_name=QE65QN90AATXXC-2021-Neo-QLED\nbuild.string=T-NKM2EDEUC-1520.7-LONG-BUILD-STRING\n\
             os=Linux 4.1.10-tizen #1 SMP PREEMPT Tue Mar 1 12:00:00 KST 2022 armv7l GNU/Linux with extra words\n\
             runtime=.NET 8.0.8-servicing.24267.1\nmanufacturer=Samsung Electronics\n",
        ));
        p.set_native(NativeFacts {
            arch: "arm".into(),
            ptr_bits: 32,
            machine: "armv7l".into(),
            glibc: "2.30".into(),
            exe: "/opt/usr/globalapps/plxnhello0/bin/dotnet-launcher-extra-long-name".into(),
        });
        p.step(Step::Egl, true, "EGL 1.4 vendor \"ARM\" version \"1.4 Midgard-r28p0-06rel0-git(6d4e2f5)\"; ES2 context and window surface created");
        p.step(Step::Gl, true, "vendor \"ARM\" renderer \"Mali-T860\" version \"OpenGL ES 3.2 v1.r28p0-06rel0.f1d3e2b5a7c9d4e6f8a0b1c2d3e4f5a6\"");
        p.step(Step::Sdl, false, "SDL2 2.32.10, SDL2_ttf 2.24.0: FAILED at gl: drawing the panel raised glError 0x0502 after the first present");
        add_probe_steps(&p, Gfx::Emulator, false);
        p
    }

    /// Ordinary device facts and the longest step-8 refusal the probe can write (`Gfx::Worst`): a first error
    /// wrapping onto a second row and a bisect line wrapping onto a second row. It is the worst case of the
    /// result itself, not of the device facts.
    fn worst_gfx_panel() -> Panel {
        let p = typical_panel();
        add_probe_steps(&p, Gfx::Worst, true);
        p
    }

    /// The panel the next emulator run is expected to show: ordinary device facts, step 8 with `plx_gfx`
    /// entered (all 14 pairs compile now), the frame count, the font's origin and the bisect, step 9
    /// answering.
    fn emulator_panel() -> Panel {
        let p = typical_panel();
        add_probe_steps(&p, Gfx::Emulator, true);
        p
    }

    /// Ordinary device facts and the longest step-8 result of a run that entered `plx_gfx` and went wrong.
    fn worst_entered_gfx_panel() -> Panel {
        let p = typical_panel();
        add_probe_steps(&p, Gfx::WorstEntered, true);
        p
    }

    /// Absurd: every free-text field as long as `clean` lets it be, in wide capitals.
    fn worst_panel() -> Panel {
        let p = typical_panel();
        let long = |c: char| c.to_string().repeat(300);
        p.set_host_facts(parse_host_facts(&format!(
            "model_name={}\nbuild.string={}\nos={}\nruntime={}\nplatform.name={}\nmanufacturer={}\n",
            long('M'), long('B'), long('O'), long('R'), long('P'), long('F')
        )));
        p.set_native(NativeFacts { arch: "x86_64".into(), ptr_bits: 64, machine: long('m'), glibc: long('g'), exe: long('e') });
        p.step(Step::Egl, true, &long('E'));
        p.step(Step::Gl, true, &long('G'));
        p.step(Step::Sdl, false, &long('S'));
        p
    }

    /// How full the plate is, against the bound that decides (`CONTENT_LIMIT`, the plate less its bottom pad),
    /// not the plate's height: the first Windows run's "1040 of 1048 px" read as a fit and was an overflow.
    fn fit(r: &Rendered) -> String {
        format!(
            "text reaches row {} of {CONTENT_LIMIT} usable ({} px of headroom; the plate is {PANEL_H} px tall with a {PAD} px bottom pad)",
            r.used_height,
            CONTENT_LIMIT - r.used_height
        )
    }

    fn dump(rendered: &Rendered, name: &str) {
        if let Ok(stem) = std::env::var("PLX_PANEL_DUMP") {
            crate::testutil::dump_bmp(&rendered.image, &format!("{stem}-{name}.bmp"));
        }
    }

    /// Rows of the image that hold bright (ink) pixels, as `(first, last)` bands separated by dark rows.
    fn ink_rows(img: &Label) -> Vec<(usize, usize)> {
        let (w, h) = (img.width as usize, img.height as usize);
        let mut bands: Vec<(usize, usize)> = Vec::new();
        for y in 0..h {
            let ink = (0..w).any(|x| img.rgba[(y * w + x) * 4..][..3].iter().any(|&c| c > 150));
            match (ink, bands.last_mut()) {
                (true, Some(b)) if b.1 + 1 == y => b.1 = y,
                (true, _) => bands.push((y, y)),
                _ => {}
            }
        }
        bands
    }

    #[test]
    fn the_typical_panel_renders_and_fits_with_every_section_inked() {
        with_renderer(|r| {
            let (_, sections) = typical_panel().snapshot();
            let rendered = r.render(&sections).expect("render");
            dump(&rendered, "typical");
            let img = &rendered.image;
            assert_eq!((img.width, img.height), (PANEL_W, PANEL_H));
            assert_eq!(img.rgba.len(), (PANEL_W * PANEL_H * 4) as usize);
            assert_eq!(&img.rgba[..4], &[PLATE_RGB[0], PLATE_RGB[1], PLATE_RGB[2], 255], "plate corner, R G B A");
            assert!(img.rgba.chunks_exact(4).all(|p| p[3] == 255), "every pixel opaque");
            assert!(!rendered.overflowed, "the typical panel must fit: {}", fit(&rendered));
            eprintln!("typical panel: {}; replaced glyphs {:?}", fit(&rendered), rendered.replaced);
            // Text is drawn: bands of ink exist and run down the plate in section order.
            let bands = ink_rows(img);
            assert!(bands.len() >= sections.len(), "{} ink bands for {} sections", bands.len(), sections.len());
            assert!(bands[0].0 < PAD as usize + 40, "the header is at the top: {:?}", bands[0]);
            assert!(bands.last().unwrap().1 < (PANEL_H - PAD / 2) as usize, "nothing touches the bottom edge");
        });
    }

    /// The panel's vertical budget depends on the row pitch really being `LINE_PITCH`: a SDL_ttf that ignored
    /// `TTF_SetFontLineSkip` would put the 3 px per row back and overflow the long panel on the TV, where no
    /// test runs. The glyphs must also stay the size the brief asks for.
    #[test]
    fn wrapped_rows_are_line_pitch_apart_and_the_glyphs_are_still_28px() {
        with_renderer(|r| {
            let one = |text: &str| r.render(&[Section { tone: Tone::Fact, text: text.to_string() }]).unwrap().used_height - PAD;
            let (a, ab, abc) = (one("HEH"), one("HEH\nHEH"), one("HEH\nHEH\nHEH"));
            eprintln!("rows: 1 = {a} px, 2 = {ab} px, 3 = {abc} px; LINE_PITCH {LINE_PITCH}");
            assert_eq!(ab - a, LINE_PITCH, "the second row sits one line pitch below the first");
            assert_eq!(abc - ab, LINE_PITCH);
            assert!(LINE_PITCH >= FONT_PX, "rows must not overlap");
            assert!(LINE_PITCH * 10 >= FONT_PX * 11, "a pitch under 1.1 em would crowd the descenders: {LINE_PITCH} px at {FONT_PX} px");
        });
    }

    #[test]
    fn body_text_is_at_least_28px_and_so_is_the_line_pitch() {
        assert!(FONT_PX >= 28, "the brief asks for >= 28 px text");
        with_renderer(|r| {
            // The ink height of a capital letter must be a sane fraction of the em (Inter's cap height
            // is ~0.73 em), so 28 px text really is ~20 px tall letters.
            let s = Section { tone: Tone::Fact, text: "HEH".to_string() };
            let rendered = r.render(&[s]).unwrap();
            let bands = ink_rows(&rendered.image);
            assert_eq!(bands.len(), 1, "{bands:?}");
            let cap = bands[0].1 - bands[0].0 + 1;
            assert!(cap >= 20 && cap <= 26, "capital height {cap}px at {FONT_PX}px");
        });
    }

    #[test]
    fn long_but_plausible_strings_still_fit_with_the_full_key_sweep() {
        with_renderer(|r| {
            let (_, sections) = long_panel().snapshot();
            let rendered = r.render(&sections).expect("render");
            dump(&rendered, "long");
            eprintln!("long panel: {}", fit(&rendered));
            assert!(!rendered.overflowed, "the long panel must fit: {}", fit(&rendered));
        });
    }

    /// The panel the next emulator run shows (`plx_gfx` entered: the inits, the three `glGetError`s, the frame
    /// count, where the font came from, the missing extensions and the bisect summary): it is the one the
    /// owner photographs, so it must fit whole.
    #[test]
    fn the_expected_emulator_panel_fits_with_plx_gfx_entered_the_frame_count_the_font_and_the_bisect_line() {
        with_renderer(|r| {
            let (_, sections) = emulator_panel().snapshot();
            let rendered = r.render(&sections).expect("render");
            dump(&rendered, "emulator");
            eprintln!("emulator panel: {}", fit(&rendered));
            assert!(!rendered.overflowed, "the emulator panel must fit: {}", fit(&rendered));
            let text: String = sections.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n");
            assert!(text.contains("OK: init_gl+text+image+blur ran; shaders 14/14 compile+link; rect drawn, text 187px, font from res/"), "{text}");
            assert!(text.contains("glGetError init/frame/swap 0/0/0; 60 frames drawn with plx_gfx; no GL ext: ARM|EXT_shader_framebuffer_fetch EXT_disjoint_timer_query"), "{text}");
            assert!(text.contains("shader bisect (+ compiles, - fails): a0- a+ b+ c+ d+ e+ f+ g+ h+ i+ k+ l+ m+ n+ o+; j bytes clean"), "{text}");
            assert!(text.contains("shader link: L1+ L2+ L3:vs- L4:link- L5:link- L5v+ i0:vs- i2+"), "{text}");
            assert!(!text.contains("first failure"), "nothing was refused: {text}");
        });
    }

    /// A run that got into `plx_gfx` and went wrong in every way the report can say at once: still whole on
    /// the plate (the verdict, the GL errors, the frames, the missing font, the late fault, `plx_gfx`'s own
    /// failure line, the extensions, the bisect), nothing cut.
    #[test]
    fn a_step_8_that_entered_plx_gfx_and_went_wrong_in_every_way_still_fits_whole() {
        with_renderer(|r| {
            let (_, sections) = worst_entered_gfx_panel().snapshot();
            let rendered = r.render(&sections).expect("render");
            dump(&rendered, "worst-entered");
            eprintln!("worst entered step-8 panel: {}", fit(&rendered));
            assert!(!rendered.overflowed, "the longest step 8 of a run that entered plx_gfx and failed must fit: {}", fit(&rendered));
            let step8 = sections.iter().find(|s| s.text.starts_with("8 gfx")).expect("step 8");
            assert!(step8.text.contains("NOT OK: init_gl+text+image+blur ran; shaders 14/14 compile+link; rect drawn, TEXT DREW 0px"), "{}", step8.text);
            assert!(step8.text.contains("glGetError init/frame/swap 0x0502/0x0506/0x0501; 7 frames drawn with plx_gfx; no font face, looked in /opt/usr/globalapps/plxnhello0/bin/appfont.ttf"), "{}", step8.text);
            assert!(step8.text.contains("3 unbound GL call(s), first glGetFramebufferAttachmentParameteriv"), "{}", step8.text);
            assert!(step8.text.contains("12 failure line(s) in plx_gfx's log, first: text prog link failed"), "{}", step8.text);
            assert!(step8.text.contains("o-; j bytes clean"), "{}", step8.text);
            assert!(step8.text.ends_with("shader link: L1:both- L2:both- L3:both- L4:both- L5:both- L5v:both- i0:both- i2:both-"), "{}", step8.text);
        });
    }

    /// The longest step-8 result still fits the plate on ordinary device facts, with every word of it in the
    /// section's text: nothing of the first error or the bisect line is cut (the owner's photo of the first
    /// emulator run had both cut).
    #[test]
    fn the_longest_step_8_result_wraps_onto_second_rows_fits_and_keeps_all_of_its_text() {
        with_renderer(|r| {
            let (_, sections) = worst_gfx_panel().snapshot();
            let rendered = r.render(&sections).expect("render");
            dump(&rendered, "worst-gfx");
            eprintln!("worst step-8 panel: {}", fit(&rendered));
            assert!(!rendered.overflowed, "the longest step-8 result must fit: {}", fit(&rendered));
            // Step 8's section is a single image whose height is its rows; render it alone to count them.
            let step8 = sections.iter().find(|s| s.text.starts_with("8 gfx")).expect("step 8");
            let rows = |text: &str| -> i32 {
                let one = Section { tone: step8.tone, text: text.to_string() };
                r.render(&[one]).unwrap().used_height - PAD
            };
            let first_row = rows("A");
            let step8_rows = 1 + (rows(&step8.text) - first_row) / LINE_PITCH;
            eprintln!("step 8 is {step8_rows} rows (first {first_row} px, then {LINE_PITCH} px each)");
            assert!(step8_rows >= 6, "the first error and the bisect line each wrapped onto a second row: {step8_rows} rows");
            // Everything in the model is in the section's text (nothing cut by the model's caps).
            assert!(step8.text.contains("'uniform' or 'varying' (+2 more log lines in the UDP log)"), "{}", step8.text);
            assert!(step8.text.contains("the driver's copy of the source differs for a0 a b c d e f g h i k l m n o"), "{}", step8.text);
            assert!(!step8.text.contains("..."), "{}", step8.text);
        });
    }

    #[test]
    fn absurd_strings_are_cut_by_the_caps_and_do_not_fault() {
        with_renderer(|r| {
            let (_, sections) = worst_panel().snapshot();
            let rendered = r.render(&sections).expect("render");
            dump(&rendered, "worst");
            eprintln!("absurd panel: {}, overflowed={}", fit(&rendered), rendered.overflowed);
            // It may overflow (300 wide capitals per field are not a real device); it must not fault, the
            // image keeps its size and stays opaque, and the clip is reported.
            assert_eq!((rendered.image.width, rendered.image.height), (PANEL_W, PANEL_H));
            assert!(rendered.image.rgba.chunks_exact(4).all(|p| p[3] == 255));
        });
    }

    #[test]
    fn the_same_text_renders_the_same_pixels_and_new_text_new_pixels() {
        with_renderer(|r| {
            let p = typical_panel();
            let (_, a) = p.snapshot();
            let first = r.render(&a).unwrap().image.rgba;
            let again = r.render(&a).unwrap().image.rgba;
            assert!(first == again, "rasterising is deterministic, so a version bump is the only reason to redraw");
            p.key_event(true, "XF86Info", 21);
            let (_, b) = p.snapshot();
            let changed = r.render(&b).unwrap().image.rgba;
            assert!(first != changed, "a new key event changes the picture");
        });
    }

    #[test]
    fn the_drawer_logic_rerenders_only_when_the_model_moved() {
        // `Drawer` needs GL; its decision is `Tracker::stale(version)`, which the model tests cover.
        // This pins that rendering a snapshot and recording ITS version leaves the panel not stale, and
        // that a write between the snapshot and the record keeps it stale (no lost update).
        let p = typical_panel();
        let mut t = Tracker::default();
        let (v, _) = p.snapshot();
        p.key_event(true, "Up", 111); // lands while the render is in flight
        t.rendered(v);
        assert!(t.stale(p.version()), "the write after the snapshot must trigger another render");
        let (v2, _) = p.snapshot();
        t.rendered(v2);
        assert!(!t.stale(p.version()));
    }

    #[test]
    fn glyphs_the_font_lacks_are_replaced_by_ascii_and_the_rest_are_kept() {
        with_renderer(|r| {
            let probe = ['\u{2014}', '\u{2713}', '\u{041F}', '\u{23EA}', '\u{23E9}', '\u{2212}'];
            let have: Vec<(char, bool)> = probe.iter().map(|&c| (c, r.has_glyph(c))).collect();
            eprintln!("Inter glyph coverage: {have:?}");
            let mut replaced = Vec::new();
            let text = format!("{} / {}", crate::panel::HEADER, crate::panel::HINT);
            let shown = displayable(|c| r.has_glyph(c), &text, &mut replaced);
            for &(c, has) in &have {
                assert_eq!(replaced.contains(&c), !has && text.contains(c), "{c:?}: font has it = {has}");
            }
            // Whatever was replaced, what remains is drawable: no character in the output is one the
            // font lacks (the fallbacks are ASCII).
            assert!(shown.chars().all(|c| c.is_ascii() || r.has_glyph(c)), "{shown}");
            // The Cyrillic sample is real text in this font (the UTF-8 proof depends on it).
            assert!(crate::panel::SAMPLE_TEXT.chars().filter(|c| !c.is_whitespace()).all(|c| r.has_glyph(c)));
        });
    }
}

#[cfg(test)]
mod pure_tests {
    use super::*;

    #[test]
    fn displayable_replaces_only_what_the_font_lacks() {
        let none = |_c: char| false;
        let all = |_c: char| true;
        let mut r = Vec::new();
        assert_eq!(displayable(all, "a \u{23EA} \u{23E9} b", &mut r), "a \u{23EA} \u{23E9} b");
        assert!(r.is_empty());
        assert_eq!(displayable(none, "CH\u{2212}\n\u{23EA} \u{23E9} \u{2014} \u{2713} \u{0416} z", &mut r), "CH-\n<< >> - ok ? z");
        assert_eq!(r.len(), 6, "{r:?}");
        // Each replaced character is listed once however often it occurs.
        let mut r2 = Vec::new();
        displayable(none, "\u{2212}\u{2212}\u{2212}", &mut r2);
        assert_eq!(r2, ['\u{2212}']);
    }
}
