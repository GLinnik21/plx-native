// Kawase 4-tap blur pass — the backdrop-blur chain's only shader (`gfx::blur_snapshot`).
//
// FOUR taps, not a Gaussian kernel, and that is the whole point on a tiler. Each tap sits on a
// DIAGONAL half-texel-aligned offset, so GL_LINEAR resolves it as a 2x2 box for free: one fetch
// buys four texels. Two passes at widening offsets over an already 4x-downsampled target give a
// penumbra far wider than the tap count suggests, and the cost is bounded by the target's own
// area (480x270 = 130K fragments, 1/16 of a full-screen pass) rather than by a kernel radius.
//
// A separable NxN Gaussian would be the textbook answer and is the wrong one here: it needs two
// passes PER axis and N fetches each, and on Midgard the render-target switch between them costs
// more than the arithmetic it saves. Passes are the budget, taps are not.
//
// `u_texel` is the offset in UV, supplied per pass (it widens between them) and CPU-folded, since
// this shader has no idea how big its source is. highp for the same reason the UV chains in
// fs_src.frag are: a fp16 UV on a 480-wide target is off by ~a quarter texel, which turns a
// symmetric 4-tap into an asymmetric one and shows as a directional smear rather than a blur.
precision mediump float;
varying highp vec2 v_cuv;
uniform sampler2D u_tex;
uniform highp vec2 u_texel;
void main(){
  vec4 c = texture2D(u_tex, v_cuv + vec2( u_texel.x,  u_texel.y));
  c += texture2D(u_tex, v_cuv + vec2(-u_texel.x,  u_texel.y));
  c += texture2D(u_tex, v_cuv + vec2( u_texel.x, -u_texel.y));
  c += texture2D(u_tex, v_cuv + vec2(-u_texel.x, -u_texel.y));
  gl_FragColor = c * 0.25;
}
