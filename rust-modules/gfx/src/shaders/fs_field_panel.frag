// THE UNDERLAY FIELD, AS A PANEL'S MATERIAL — the page under a popover, carried into the popover
// WHERE it is: the field's own window at the panel's screen rect, inside the panel's rounded shape.
//
// This is what replaced the real backdrop blur under a cached popover (`widgets::panel_ground`).
// A blur buys letter-scale structure softened; under a panel frosted at `PANEL_MATERIAL` density
// almost none of that survives, and what does is exactly what a 15x8 field already holds — the
// colour of the page and where it is. So the panel pays one fetch here instead of a snapshot chain.
//
// Why a program of its own and not a mode of `fs_field.frag`: that one is drawn over the whole
// screen as every modal dim, and neither the sub-rect UV nor the rounded-corner SDF below may cost
// those 2M fragments anything — not even a uniform branch (`gfx::glsl_dithered`'s note on what a
// branch costs on Midgard). Here both are paid over a panel's area only.
//
// `u_uvrect` is the panel's rect in the FIELD's own space — (x, y, w, h) / screen size — so a
// green cell under the panel's bottom-left stays under its bottom-left. The field texture is
// CLAMP_TO_EDGE, and the rect never leaves [0,1] for a panel on screen.
//
// The shape: `fs_src.frag`'s `sdBox` and its INTERIOR EARLY-OUT, verbatim in intent — most of a
// panel's fragments are solidly inside, and those skip the SDF. Coverage goes in ALPHA only
// (`fs_src.frag`'s note on why multiplying the colour too draws a dark ring on a light fill).
//
// PRECISION: highp for every coordinate, `fs_src.frag`'s reason — fp16 on a panel-sized quad
// wobbles the SDF along a straight edge.
precision mediump float;
varying highp vec2 v_uv;
uniform sampler2D u_tex;
uniform vec4 u_tint;
uniform highp vec4 u_uvrect; // (origin.xy, size.zw) of the panel in field UV
uniform highp vec2 u_size;   // the panel, px
uniform highp float u_radius;
highp float sdBox(highp vec2 p, highp vec2 b, highp float r){
  highp vec2 q = abs(p) - b + vec2(r);
  return length(max(q,0.0)) + min(max(q.x,q.y),0.0) - r;
}
void main(){
  vec4 c = texture2D(u_tex, u_uvrect.xy + v_uv * u_uvrect.zw);
  highp vec2 p = (v_uv - 0.5) * u_size;
  highp vec2 hsz = u_size * 0.5;
  highp vec2 inner = hsz - vec2(u_radius + 2.0);
  float cov = 1.0;
  if (abs(p.x) >= inner.x || abs(p.y) >= inner.y) {
    cov = 1.0 - smoothstep(-1.0, 1.0, sdBox(p, hsz, u_radius));
  }
  gl_FragColor = vec4(plx_dither(c.rgb * u_tint.rgb), cov * c.a * u_tint.a);
}
