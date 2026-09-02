// The FADE variant is a SEPARATE program bound only by draw_text_fade (callers: TextView::fade_last
// - the About card's and person header's MORE dissolve - and TextView::edge_fade - a scrolling
// viewport's clipped edge, `ui::person_bio`'s bio panel): u_tfade = (from, to) in string-texture
// uv.x fades the glyph HORIZONTALLY; u_vfadeT / u_vfadeB = (from, to) in ABSOLUTE LOGICAL SCREEN Y
// fade it VERTICALLY, ramping 0->1 rising through the top band and 1->0 falling through the bottom
// one. Each band is a uniform pair rather than a bespoke uniform per caller so the three fades can
// combine (a line that is both the truncated last one AND crossing a viewport edge dissolves on
// both axes at once). All three are OFF by default ((0,0), since the gate is `to > from`) and an
// off band costs one scalar compare, not a texture sample — kept off fs_text.frag so every ordinary
// glyph on this fill-rate-bound panel doesn't pay for any of it.
precision mediump float;
varying highp vec2 v_tuv;
varying highp float v_ly;
uniform sampler2D u_tex;
uniform vec4 u_tcol;
uniform vec2 u_tfade;
uniform vec2 u_vfadeT;
uniform vec2 u_vfadeB;
void main(){ float a=texture2D(u_tex,v_tuv).a;
  if (u_tfade.y > u_tfade.x) a *= 1.0 - smoothstep(u_tfade.x, u_tfade.y, v_tuv.x);
  if (u_vfadeT.y > u_vfadeT.x) a *= smoothstep(u_vfadeT.x, u_vfadeT.y, v_ly);
  if (u_vfadeB.y > u_vfadeB.x) a *= 1.0 - smoothstep(u_vfadeB.x, u_vfadeB.y, v_ly);
  gl_FragColor=vec4(u_tcol.rgb, u_tcol.a*a); }
