// The tile texture shader is the whole CARD COMPOSITE - texture + the 1px focus edge-sheen
// (u_rimw/u_rimcol) + a soft SYMMETRIC drop-shadow (u_ch/u_shinv/u_shcol) - all in ONE pass.
// Perf (Mali-T820, per the perf review): (1) an INTERIOR EARLY-OUT - ~85% of a card's fragments
// are strictly inside the rounded rect (d < -2) where rim/AA/shadow are all zero, so they skip the
// 4 smoothsteps; on this per-thread tiler the branch genuinely saves the ALU. (2) UV remap +
// card-local p are interpolated varyings, not per-fragment math (see vs_img.vert). (3) the
// uniform-only terms (card half-size u_ch, shadow u_shinv = 0.5/blur) are folded on the CPU
// (Midgard has no uniform pre-shader). (4) the 1px rim is a single-op triangle - and its width
// must stay <=1px: the triangle hits exactly 0 at d=-2, which is what makes the d<-2 early-out
// seamless (a wider rim would be hard-cut there). The shadow sh = smoothstep(clamp(0.5 -
// d/(2*blur))) is algebraically identical to 1 - smoothstep(-blur, blur, d). rgb = tex*m
// premultiplies coverage (so a rounded texture's ~1px AA edge is very slightly darker under
// straight-alpha blend - accepted). Full-screen art (radius 0) takes the flat fast-path.
// v_cuv/v_p and the SDF chain are highp - see the PRECISION note in fs_src.frag.
precision mediump float;
varying highp vec2 v_cuv;
varying highp vec2 v_p;
uniform sampler2D u_tex;
uniform vec4 u_tint;
uniform highp float u_iradius;
uniform float u_rimw;
uniform vec4 u_rimcol;
uniform highp vec2 u_ch;
uniform float u_shinv;
uniform vec4 u_shcol;
highp float sdBox(highp vec2 p, highp vec2 b, highp float r){ highp vec2 q=abs(p)-b+vec2(r);
  return length(max(q,0.0))+min(max(q.x,q.y),0.0)-r; }
void main(){
  vec4 c = texture2D(u_tex, v_cuv);
  vec3 tex = c.rgb*u_tint.rgb;
  float ta = c.a*u_tint.a;
  if (u_iradius < 0.5) { gl_FragColor = vec4(tex, ta); return; }
  float d = sdBox(v_p, u_ch, u_iradius);
  if (d < -2.0) { gl_FragColor = vec4(tex, ta); return; }
  float m = 1.0 - smoothstep(-1.0, 1.0, d);
  float rim = max(0.0, 1.0 - abs(d + u_rimw)) * u_rimcol.a;
  tex = mix(tex, u_rimcol.rgb, rim);
  float sh = clamp(0.5 - d*u_shinv, 0.0, 1.0);
  sh = sh*sh*(3.0 - 2.0*sh) * u_shcol.a * (1.0 - m);
  gl_FragColor = vec4(tex*m, ta*m + sh);
}
