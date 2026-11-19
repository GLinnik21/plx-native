//! H5, GL half: upload the SDL-rendered label as a GLES2 texture and draw it as a textured quad on
//! the existing frame. Everything here runs on the render thread with the EGL context current.
//!
//! The GL entry points are their own table (`LabelSyms`), loaded from the same libGLESv2 handle as
//! `sym::Gles`, so a missing one can only switch the label off (logged by name), never fail the
//! step-1 window and colour test.
//!
//! The label is an OPAQUE plate (the SDL side blits the text onto a dark plate), so no blending is
//! needed and alpha-channel handling of the compositor cannot hide or tint it. The texture is the
//! label's own pixel size; the quad is scaled by `screen_height / 1080`, so it looks the same on a
//! 1080p and a 4K panel.

use crate::dl::symtable;
use crate::h5::Label;
use crate::log::plog;
use std::ffi::{c_char, c_void, CString};

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE0: u32 = 0x84C0;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_FLOAT: u32 = 0x1406;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_TRIANGLE_STRIP: u32 = 0x0005;

const VERTEX_SRC: &str = "attribute vec2 a_pos;\nattribute vec2 a_uv;\nvarying vec2 v_uv;\n\
void main() { gl_Position = vec4(a_pos, 0.0, 1.0); v_uv = a_uv; }\n";
const FRAGMENT_SRC: &str = "precision mediump float;\nvarying vec2 v_uv;\nuniform sampler2D u_tex;\n\
void main() { gl_FragColor = texture2D(u_tex, v_uv); }\n";

symtable! {
    /// The libGLESv2 entry points the label needs (all core GLES 2.0).
    pub struct LabelSyms {
        required {
            gen_textures: unsafe extern "C" fn(i32, *mut u32) = "glGenTextures",
            bind_texture: unsafe extern "C" fn(u32, u32) = "glBindTexture",
            tex_image_2d: unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void) = "glTexImage2D",
            tex_parameteri: unsafe extern "C" fn(u32, u32, i32) = "glTexParameteri",
            pixel_storei: unsafe extern "C" fn(u32, i32) = "glPixelStorei",
            active_texture: unsafe extern "C" fn(u32) = "glActiveTexture",
            delete_textures: unsafe extern "C" fn(i32, *const u32) = "glDeleteTextures",
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
            use_program: unsafe extern "C" fn(u32) = "glUseProgram",
            get_uniform_location: unsafe extern "C" fn(u32, *const c_char) -> i32 = "glGetUniformLocation",
            uniform1i: unsafe extern "C" fn(i32, i32) = "glUniform1i",
            vertex_attrib_pointer: unsafe extern "C" fn(u32, i32, u32, u8, i32, *const c_void) = "glVertexAttribPointer",
            enable_vertex_attrib_array: unsafe extern "C" fn(u32) = "glEnableVertexAttribArray",
            disable_vertex_attrib_array: unsafe extern "C" fn(u32) = "glDisableVertexAttribArray",
            draw_arrays: unsafe extern "C" fn(u32, i32, i32) = "glDrawArrays",
            get_error: unsafe extern "C" fn() -> u32 = "glGetError",
        }
        optional {}
    }
}

/// The label's GL objects.
pub struct GlLabel {
    s: LabelSyms,
    program: u32,
    texture: u32,
    pub width: i32,
    pub height: i32,
}

impl GlLabel {
    fn drain_errors(s: &LabelSyms) -> u32 {
        let mut last = 0;
        for _ in 0..16 {
            // SAFETY: a current context exists on this thread.
            let e = unsafe { (s.get_error)() };
            if e == 0 {
                break;
            }
            last = e;
        }
        last
    }

    fn compile(s: &LabelSyms, kind: u32, src: &str) -> Result<u32, String> {
        let name = if kind == GL_VERTEX_SHADER { "vertex" } else { "fragment" };
        // SAFETY (whole function): current context; `csrc` outlives the calls; the log buffer is
        // 1 KiB and its size is passed.
        let shader = unsafe { (s.create_shader)(kind) };
        if shader == 0 {
            return Err(format!("glCreateShader({name}) returned 0"));
        }
        let csrc = CString::new(src).map_err(|_| "shader source has a NUL".to_string())?;
        let ptr = csrc.as_ptr();
        let mut ok = 0i32;
        unsafe {
            (s.shader_source)(shader, 1, &ptr, std::ptr::null());
            (s.compile_shader)(shader);
            (s.get_shaderiv)(shader, GL_COMPILE_STATUS, &mut ok);
        }
        if ok == 0 {
            let mut buf = [0 as c_char; 1024];
            let mut n = 0i32;
            unsafe { (s.get_shader_info_log)(shader, buf.len() as i32, &mut n, buf.as_mut_ptr()) };
            let text = log_text(&buf, n);
            unsafe { (s.delete_shader)(shader) };
            return Err(format!("{name} shader did not compile: {text}"));
        }
        plog!("H5 gl: {name} shader compiled (id {shader})");
        Ok(shader)
    }

    /// Compile the shaders, link the program and upload `label` as an RGBA texture. Render thread.
    pub fn create(s: LabelSyms, label: &Label) -> Result<GlLabel, String> {
        let stale = Self::drain_errors(&s);
        if stale != 0 {
            plog!("H5 gl: cleared a pending glError 0x{stale:04x} left by earlier code");
        }
        let vs = Self::compile(&s, GL_VERTEX_SHADER, VERTEX_SRC)?;
        let fs = Self::compile(&s, GL_FRAGMENT_SHADER, FRAGMENT_SRC)?;
        // SAFETY (rest of the function): current context on this thread, handles from the calls
        // above, `rgba` is `width * height * 4` bytes (asserted by the caller's Label invariant).
        let program = unsafe { (s.create_program)() };
        if program == 0 {
            return Err("glCreateProgram returned 0".to_string());
        }
        unsafe {
            (s.attach_shader)(program, vs);
            (s.attach_shader)(program, fs);
            (s.bind_attrib_location)(program, 0, c"a_pos".as_ptr());
            (s.bind_attrib_location)(program, 1, c"a_uv".as_ptr());
            (s.link_program)(program);
        }
        let mut ok = 0i32;
        unsafe { (s.get_programiv)(program, GL_LINK_STATUS, &mut ok) };
        if ok == 0 {
            let mut buf = [0 as c_char; 1024];
            let mut n = 0i32;
            unsafe { (s.get_program_info_log)(program, buf.len() as i32, &mut n, buf.as_mut_ptr()) };
            return Err(format!("program did not link: {}", log_text(&buf, n)));
        }
        unsafe {
            (s.delete_shader)(vs);
            (s.delete_shader)(fs);
        }
        plog!("H5 gl: program linked (id {program})");

        let mut texture = 0u32;
        let (w, h) = (label.width, label.height);
        debug_assert_eq!(label.rgba.len(), (w as usize) * (h as usize) * 4);
        unsafe {
            (s.gen_textures)(1, &mut texture);
            (s.active_texture)(GL_TEXTURE0);
            (s.bind_texture)(GL_TEXTURE_2D, texture);
            (s.pixel_storei)(GL_UNPACK_ALIGNMENT, 4);
            (s.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            (s.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
            (s.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            (s.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
            (s.tex_image_2d)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as i32,
                w,
                h,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                label.rgba.as_ptr().cast(),
            );
        }
        let err = Self::drain_errors(&s);
        plog!("H5 gl: texture {texture} uploaded {w}x{h} RGBA ({} bytes), glError=0x{err:04x}", label.rgba.len());
        if texture == 0 || err != 0 {
            return Err(format!("texture upload failed (id {texture}, glError 0x{err:04x})"));
        }
        Ok(GlLabel { s, program, texture, width: w, height: h })
    }

    /// Draw the label centred. Render thread, after the clear, before the swap. Returns the first
    /// `glGetError` seen (0 = fine).
    pub fn draw(&self, screen_w: i32, screen_h: i32) -> u32 {
        let s = &self.s;
        let scale = screen_h as f32 / 1080.0;
        let w = self.width as f32 * scale / screen_w as f32 * 2.0;
        let h = self.height as f32 * scale / screen_h as f32 * 2.0;
        let (x0, x1, y0, y1) = (-w / 2.0, w / 2.0, -h / 2.0, h / 2.0);
        // x, y, u, v: a strip TL, BL, TR, BR. Row 0 of the texture is the TOP of the label (SDL's
        // order) and GL's first uploaded row is v = 0, so the top vertices carry v = 0.
        let verts: [f32; 16] = [x0, y1, 0.0, 0.0, x0, y0, 0.0, 1.0, x1, y1, 1.0, 0.0, x1, y0, 1.0, 1.0];
        // SAFETY: current context; client-side vertex arrays (valid in GLES2 with no buffer bound)
        // point at `verts`, which outlives the draw call.
        unsafe {
            (s.use_program)(self.program);
            (s.active_texture)(GL_TEXTURE0);
            (s.bind_texture)(GL_TEXTURE_2D, self.texture);
            let loc = (s.get_uniform_location)(self.program, c"u_tex".as_ptr());
            (s.uniform1i)(loc, 0);
            (s.vertex_attrib_pointer)(0, 2, GL_FLOAT, 0, 16, verts.as_ptr().cast());
            (s.vertex_attrib_pointer)(1, 2, GL_FLOAT, 0, 16, verts[2..].as_ptr().cast());
            (s.enable_vertex_attrib_array)(0);
            (s.enable_vertex_attrib_array)(1);
            (s.draw_arrays)(GL_TRIANGLE_STRIP, 0, 4);
            (s.disable_vertex_attrib_array)(0);
            (s.disable_vertex_attrib_array)(1);
            (s.get_error)()
        }
    }

    /// Release the GL objects. Render thread, context still current.
    pub fn destroy(&self) {
        // SAFETY: handles made by `create` on this context.
        unsafe {
            (self.s.delete_textures)(1, &self.texture);
            (self.s.delete_program)(self.program);
        }
        plog!("H5 gl: label texture and program deleted");
    }
}

fn log_text(buf: &[c_char], n: i32) -> String {
    let n = (n.max(0) as usize).min(buf.len());
    let bytes: Vec<u8> = buf[..n].iter().map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

/// Load the label's GL table next to `Gles`; the caller drops the label on `Err`.
pub fn load_syms(gles_lib: &crate::dl::Lib) -> Result<LabelSyms, String> {
    LabelSyms::load(&[gles_lib]).map_err(|missing| format!("missing GLES2 entry points: {}", missing.join(", ")))
}
