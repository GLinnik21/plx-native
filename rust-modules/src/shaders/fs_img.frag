// The tile texture shader is the whole CARD COMPOSITE - texture + the 1px focus edge-sheen
// (u_rimw/u_rimcol) + a soft SYMMETRIC drop-shadow (u_card/u_shinv/u_shcol) - all in ONE pass.
// Perf (Mali-T820, per the perf review): (1) an INTERIOR EARLY-OUT - ~85% of a card's fragments
// are strictly inside the rounded rect (d < -2) where rim/AA/shadow are all zero, so they skip the
// 4 smoothsteps; on this per-thread tiler the branch genuinely saves the ALU. (2) UV remap +
// card-local p are interpolated varyings, not per-fragment math (see vs_img.vert). (3) the
// uniform-only terms (u_card's corner coordinates and interior cutoff, u_shinv = 0.5/blur) are folded on the CPU
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
#ifndef PLX_STILL_GROUND
uniform vec4 u_tint;
#endif
uniform float u_rimw;
uniform vec4 u_rimcol;
uniform highp vec4 u_card; // half-size minus radius, radius, conservative interior threshold
uniform float u_shinv;
uniform vec4 u_shcol;
#ifdef PLX_STILL_GROUND
varying mediump float v_still_ramp;
uniform vec4 u_still_col;
// Compose the existing straight-alpha card output, then its separate SDF-clipped scrim.
// Emit premultiplied RGB for this specialized draw's GL_ONE blend: no divide or alpha branch
// enlarges the whole program's register budget. Zero/tiny alpha needs no special case.
// The near-black ink is a supplied theme color; treating it as zero changes the picture.
vec4 stillOver(vec3 rgb, float alpha, float coverage, float ramp){
  float s = ramp * coverage;
  float keep = alpha * (1.0 - s);
  return vec4(rgb * keep + u_still_col.rgb * s, s + keep);
}
#endif
void main(){
  vec4 c = texture2D(u_tex, v_cuv);
#ifdef PLX_STILL_GROUND
  // The CPU admits only an exactly-white tint to this specialization.
  vec3 tex = c.rgb;
  float ta = c.a;
#else
  vec3 tex = c.rgb*u_tint.rgb;
  float ta = c.a*u_tint.a;
#endif
#ifdef PLX_STILL_GROUND
  float ramp = u_still_col.a * clamp(v_still_ramp, 0.0, 1.0);
  if (u_card.z < 0.5) { gl_FragColor = stillOver(tex, ta, 1.0, ramp); return; }
#else
  if (u_card.z < 0.5) { gl_FragColor = vec4(tex, ta); return; }
#endif
  highp vec2 q = abs(v_p) - u_card.xy;
  highp float straight = max(q.x, q.y);
  if (straight < u_card.w) {
#ifdef PLX_STILL_GROUND
    gl_FragColor = stillOver(tex, ta, 1.0, ramp);
#else
    gl_FragColor = vec4(tex, ta);
#endif
    return;
  }
  float d = straight - u_card.z;
  if (min(q.x, q.y) > 0.0) d = length(q) - u_card.z;
#ifdef PLX_STILL_GROUND
  if (d < -2.0) { gl_FragColor = stillOver(tex, ta, 1.0, ramp); return; }
#else
  if (d < -2.0) { gl_FragColor = vec4(tex, ta); return; }
#endif
  float m = 1.0 - smoothstep(-1.0, 1.0, d);
  float rim = max(0.0, 1.0 - abs(d + u_rimw)) * u_rimcol.a;
  tex = mix(tex, u_rimcol.rgb, rim);
  float sh = clamp(0.5 - d*u_shinv, 0.0, 1.0);
  sh = sh*sh*(3.0 - 2.0*sh) * u_shcol.a * (1.0 - m);
#ifdef PLX_STILL_GROUND
  gl_FragColor = stillOver(tex*m, ta*m + sh, m, ramp);
#else
  gl_FragColor = vec4(tex*m, ta*m + sh);
#endif
}
