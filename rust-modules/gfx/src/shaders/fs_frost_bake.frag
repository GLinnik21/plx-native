// THE FROST'S BODY BAKE — the menu body's dim and ceiling, applied ONCE to the retained frost at its own
// 1/8 resolution (`gfx::FrameCache::frost_bake_owed`) instead of per pixel of the glass quad on every
// frame the menu stands (`fs_glass.frag`, which under PLX_FROST now reads the baked texel and does
// neither).
//
// WHY. Measured on the television (Mali, `glass.frozen`, the settled open, M GPU cycles): the frost
// composite was 5.58M against 4.62M for the latched field it replaced. The ceiling cost 0.69M of it:
// 0.38M executing and 0.31M paid even when its uniform switched it off (the code's registers and its
// branch are the cost, not the work), and the body's dim fetch, a second field read beside the frost
// texture, cost 0.27M. A composite with neither measured 4.23M. This program is where both went: a
// 240x135 target at the full canvas, 32 thousand fragments, run when the frost's snapshot or the
// frame's dim changes (every frame of the dim's own ramp; once per open at rest).
//
// THE ORDER IS PINNED and it is the one the composite had: DIM, then CAP, then (in the glass) the face.
// A body capped BEFORE the dim would let the dim's ink raise it past the ceiling the ink floors were
// derived from (`ui::theme::FROST_BODY_CEILING`; the tertiary-ink test grades the order).
//
// The dim is `mix(c, ink, a)` with `ink = u_dim.rgb` times the field texel at the screen point when
// `u_dim_field` says so (`fs_glass.frag`'s `dimRGB`, modes 0 and 1: a body is never the field's own
// texel, mode 2). `v_cuv` is the frost texel's own UV (the pass un-flips its quad so the body is stored
// the way the frost is), and `u_dim_uv` is `gfx::snapshot_to_screen` of the frost's rect, the mapping the
// glass's own sharp-source dim reads the same field through.
//
// THE CEILING is `fs_glass.frag`'s former `u_frost_cap` block, statement for statement: `(knee,
// headroom)`; the brightest CHANNEL is held under `knee + headroom` by `knee + headroom * o / (o +
// headroom)` (`o` the excess over the knee); the whole texel scales by the capped/uncapped ratio, so hue
// survives. Headroom 0 = no cap. The branches are uniform and coherent, and this pass is 1/64 of a
// full-screen quad, so unlike the composite it can afford them.
//
// The body is OPAQUE: the glass reads `.rgb` only and its alpha is the tint's.
precision mediump float;
varying highp vec2 v_cuv;
uniform sampler2D u_tex;
uniform sampler2D u_dim_tex;
uniform vec4 u_dim;
uniform highp vec4 u_dim_uv;
uniform float u_dim_field;
uniform vec2 u_frost_cap;
void main(){
  vec3 c = texture2D(u_tex, v_cuv).rgb;
  if (u_dim.a > 0.0) {
    vec3 ink = u_dim.rgb;
    float a = u_dim.a;
    if (u_dim_field > 0.0) {
      vec4 f = texture2D(u_dim_tex, v_cuv * u_dim_uv.xy + u_dim_uv.zw);
      ink *= f.rgb;
      a *= f.a;
    }
    c = mix(c, ink, a);
  }
  if (u_frost_cap.y > 0.0) {
    float m = max(c.r, max(c.g, c.b));
    float o = max(m - u_frost_cap.x, 0.0);
    float y = min(m, u_frost_cap.x) + u_frost_cap.y * (o / (o + u_frost_cap.y));
    c *= y / max(m, 0.0001);
  }
  gl_FragColor = vec4(c, 1.0);
}
