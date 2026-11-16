// The STRAIGHTEN pass (`gfx/straighten.rs`): the frame's premultiplied colour turned into the straight
// colour the television's compositor multiplies by alpha. `rgb' = a > 0 ? rgb / a : 0`, alpha as it is.
//
// WHY. Over the hardware video plane the platform composites our surface as `ui.rgb * ui.a + video *
// (1 - ui.a)` (measured on the dev set: GRAPHIC (57, 64, 70, a 0.275), VIDEO (226, 213, 51), DISPLAY
// (182, 172, 55); straight alpha predicts (180, 172, 56), premultiplied (221, 218, 107)). The
// framebuffer holds `(rgb * a, a)` (`gfx.rs`'s blend note), so every translucent pixel would be
// multiplied by its own alpha a second time. Dividing it out here makes the compositor's multiply
// return the colour the frame drew: `(rgb / a) * a + video * (1 - a)` is `rgb + video * (1 - a)`.
//
// ONE SOURCE, THREE PROGRAMS, picked by the prefix `gfx::straighten` puts in front of it:
//   PLX_STRAIGHTEN_ARM   GL_ARM_shader_framebuffer_fetch  - the colour already in the tile, read in place
//   PLX_STRAIGHTEN_EXT   GL_EXT_shader_framebuffer_fetch  - the same read under the Khronos-era name
//   (neither)            a copy of the frame, sampled 1:1 - for a GPU with no fetch (the simulator)
// The two fetch programs read the pixel from tile memory: no copy, no second render pass, no memory
// traffic beyond what the frame already does. The copy program is the fallback and costs a copy of the
// frame plus a pass reload.
//
// `a == 0` writes zero colour and not `rgb / 0`: the compositor ignores such a pixel, but a capture of
// the graphics plane should read it as the nothing it is. A colour above its alpha (an additive rim)
// saturates at the 8-bit write, which is all a straight picture can say for it.
//
// PRECISION: the divide runs in highp (the fragment stage's highp is established on this device:
// `fs_field_panel.frag` declares highp varyings and uniforms). An fp16 `1 / a` followed by an fp16
// product can land half a code off at the top of the range, and the divide is this program's whole job.
// What this file cannot choose is the precision of the value the fetch returns (`gl_LastFragColorARM`
// and `gl_LastFragData` are mediump in their extensions: an 8-bit value rounded to fp16, within
// ~0.13 code at 1.0) or of `gl_FragColor`; the f32 round-trip bound in `gfx/straighten.rs`'s tests is
// the model's, and the pass's real error is a code-level quantity only a TV capture can state.
#if defined(PLX_STRAIGHTEN_ARM)
#extension GL_ARM_shader_framebuffer_fetch : require
#define PLX_BEHIND gl_LastFragColorARM
#elif defined(PLX_STRAIGHTEN_EXT)
#extension GL_EXT_shader_framebuffer_fetch : require
#define PLX_BEHIND gl_LastFragData[0]
#endif
precision mediump float;
varying highp vec2 v_uv;
#if !defined(PLX_STRAIGHTEN_ARM) && !defined(PLX_STRAIGHTEN_EXT)
uniform sampler2D u_tex;
#endif
void main(){
#if defined(PLX_STRAIGHTEN_ARM) || defined(PLX_STRAIGHTEN_EXT)
  highp vec4 c = PLX_BEHIND;
#else
  highp vec4 c = texture2D(u_tex, v_uv);
#endif
  highp float k = c.a > 0.0 ? 1.0 / c.a : 0.0;
  gl_FragColor = vec4(c.rgb * k, c.a);
}
