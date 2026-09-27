// THE UNDERLAY FIELD — a coarse colour field of whatever is rendered beneath an overlay, drawn as
// one bilinear-magnified texture so the overlay INHERITS the light of the content under it and
// inherits it SPATIALLY (green stays where the green is).
//
// It is deliberately the smallest fragment program in the app: one fetch, one multiply, one shared
// dither. The blur it replaces is not approximated here — it was PAID on the CPU/GPU once, when
// `gfx::field_kick` reduced the framebuffer to 15x8 cells and `ui::underlay` low-passed
// and reconstructed them into a 60x32 texture. What is left at draw time is a magnification, and
// GL_LINEAR on a 60x32 source over a 1920x1080 quad is the whole of the gradient.
//
// THE DITHER IS NOT OPTIONAL HERE and it is the reason this program carries `dither.glsl` at all:
// a field reconstructed from a 15x8 grid is the slowest ramp this app produces — hundreds of rows
// per 8-bit code — which is exactly the staircase that prelude's header measured on the panel.
// `gfx::dither_for_field` is the policy; see `gfx::glsl_dithered!`.
//
// `u_tint` is a straight multiply and carries the painter's alpha cascade: rgb grades the field
// (a `Role::Dim` hands it the scrim ink pre-mixed on the CPU) and `.a` is the coverage.
//
// The vertex half is `vs_src.vert` — `v_uv` is the unit quad, which is exactly the texture
// coordinate for a field drawn 1:1 over its rect, so no `u_uvrect` and no second varying.
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_tex;
uniform vec4 u_tint;
void main(){
  vec4 c = texture2D(u_tex, v_uv);
  gl_FragColor = vec4(plx_dither(c.rgb * u_tint.rgb), c.a * u_tint.a);
}
