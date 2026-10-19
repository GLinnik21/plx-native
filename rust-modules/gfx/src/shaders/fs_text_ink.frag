// The INK variant of the text program, a SEPARATE program bound only while an ink clip is set
// (`text::set_ink_clip`, opened by `ui::widgets::paint_inked`) - so a label a capsule's rounded end
// crosses is painted in each ink exactly where the capsule is. The plain program (fs_text.frag) pays
// nothing for it. Each call paints ONE ink: its alpha is multiplied by the coverage of up to two
// rounded-rect shapes in ABSOLUTE LOGICAL SCREEN px (the space `u_trect` is passed in; v_lx / v_ly,
// highp per `ui/CLAUDE.md`'s Mali rule), each kept INSIDE or OUTSIDE, so the regions "inside the
// focus capsule", "inside the selection plate and outside it" and "outside both" tile the glyph
// pixel by pixel, one coverage AA'd over half a pixel either side of the arc. A shape with
// u_mp keep == 0 is off and costs one compare. Alpha is scaled, never `discard`ed.
// u_ma / u_mb = (x0, y0, x1, y1); u_mp = (radius A, keep A, radius B, keep B), keep +1 inside,
// -1 outside, 0 off.
precision mediump float;
varying highp vec2 v_tuv;
varying highp float v_lx;
varying highp float v_ly;
uniform sampler2D u_tex;
uniform vec4 u_tcol;
uniform highp vec4 u_ma;
uniform highp vec4 u_mb;
uniform highp vec4 u_mp;
highp float shape_cover(highp vec4 r, highp float rad) {
  highp vec2 c = (r.xy + r.zw) * 0.5;
  highp vec2 q = abs(vec2(v_lx, v_ly) - c) - ((r.zw - r.xy) * 0.5 - rad);
  highp float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - rad;
  return clamp(0.5 - d, 0.0, 1.0);
}
void main(){ float a=texture2D(u_tex,v_tuv).a;
  if (u_mp.y != 0.0) { float c = shape_cover(u_ma, u_mp.x); a *= u_mp.y > 0.0 ? c : 1.0 - c; }
  if (u_mp.w != 0.0) { float c = shape_cover(u_mb, u_mp.z); a *= u_mp.w > 0.0 ? c : 1.0 - c; }
  gl_FragColor=vec4(u_tcol.rgb, u_tcol.a*a); }
