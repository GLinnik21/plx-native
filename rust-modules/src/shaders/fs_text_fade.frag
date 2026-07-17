// The FADE variant is a SEPARATE program bound only by draw_text_fade (one caller - the About
// card's MORE): u_tfade = (from, to) in string-texture uv.x fades the glyph alpha 1->0 across
// that band. Kept off the shared fs_text.frag so every ordinary glyph doesn't pay the
// per-fragment smoothstep on this fill-rate-bound panel. highp v_tuv: see fs_text.frag.
precision mediump float;
varying highp vec2 v_tuv;
uniform sampler2D u_tex;
uniform vec4 u_tcol;
uniform vec2 u_tfade;
void main(){ float a=texture2D(u_tex,v_tuv).a;
  a *= 1.0 - smoothstep(u_tfade.x, u_tfade.y, v_tuv.x);
  gl_FragColor=vec4(u_tcol.rgb, u_tcol.a*a); }
