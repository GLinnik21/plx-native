// THE ROUNDED CLIP — prepended (`gfx::glsl_rclip!`) to the MASKED variants of the per-rect programs,
// `fs_src.frag` and `fs_tex.frag`, which are bound only while `gfx::rclip_set` holds a rounded clip.
// It is a coverage mask in the alpha, so a draw wears the arc of a rounded rect the way glyphs wear
// an ink clip's (`fs_text_ink.frag`): the glass menu's rows are cut by the lens's round edge instead
// of a rectangle inscribed in it.
//
// THE COORDINATE IS AUTHORED PX, FROM A VARYING. The vertex shaders (`vs_src.vert`, `vs_img.vert`)
// emit `v_px` under `PLX_RCLIP`, the px they already compute and divide by `u_screen`, so the mask is
// independent of the bound target (the frame, a region fill, the reduced blur source) and of the
// simulator's letterbox and scale: no inverse of the viewport map, nothing re-uploaded when the
// target changes. NOT `gl_FragCoord` (cost rule 4, `dither.glsl`: arithmetic on it measured 45 fps
// against 60 on this GPU) and no derivative (nothing here links `OES_standard_derivatives`).
//
// THE COST RULE (`dither.glsl` rule 1): a separate program, never a uniform branch in the plain one.
// The plain programs are byte-identical to what shipped (the test
// `the_plain_rect_and_texture_programs_carry_nothing_of_the_rounded_clip`); this is bound only inside
// a rounded scope, ~350 ms per menu open or close.
//
// THE MASK is `fs_text_ink.frag`'s `shape_cover`, in the folded form the CPU sends:
//   u_rc = (centre.xy, half extent - radius .zw)   u_rr = radius, held to the half extents
// (`gfx::rclip_fold`; `gfx::rclip_cover` is the CPU mirror the tests grade). 0.5 on the edge, 1 half
// a pixel in, 0 half a pixel out; the glass's own edge (`fs_glass.frag`'s `cov`) is 0.5 there too.
//
// BLEND. Every masked fragment shader emits STRAIGHT colour (the default blend is SRC_ALPHA /
// ONE_MINUS_SRC_ALPHA), so coverage multiplies the alpha only and the colour is left alone.
//
// This sits before the file's own `precision` statement, so every declaration names its precision.
varying highp vec2 v_px;
uniform highp vec4 u_rc;
uniform highp float u_rr;
highp float rclip_cover(highp vec2 p) {
  highp vec2 q = abs(p - u_rc.xy) - u_rc.zw;
  highp float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - u_rr;
  return clamp(0.5 - d, 0.0, 1.0);
}
mediump vec4 rclip_out(mediump vec4 c) { return vec4(c.rgb, c.a * rclip_cover(v_px)); }
