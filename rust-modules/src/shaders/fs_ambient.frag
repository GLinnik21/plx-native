// 4-corner bilinear gradient. TWO roles share this program because they are one field: the opaque
// full-screen ambient wash (`gfx::draw_ambient`, which forces every corner's alpha to 1.0) and the
// alpha-carrying corner gradient that sits OVER artwork (`gfx::draw_grad4` — the hero text scrim).
// Alpha is interpolated with the colour, so the four corners must share an rgb: straight
// (non-premultiplied) rgba only interpolates exactly when they do. One ink at four alphas, never
// four hues at four alphas.
//
// ONE MIX PER FRAGMENT. The horizontal corner mixes are varyings from `vs_ambient.vert` — exact,
// because they are linear in u and a varying interpolates a linear function exactly — so this
// side only blends top against bottom. The three-mix form of this shader priced the hero's corner
// scrim at 3.2M GPU cycles a frame on the set (2026-09-02), and the fold's full-screen wash more.
//
// PRECISION: colour-only, no edges, no texture, so the coordinate is mediump and the whole mix runs
// in fp16. A highp coordinate was tried here (2026-09-01, against contours seen on a slow wash) and
// it is what the dither below actually addresses, not the interpolation: an fp16 uv steps by about
// 1/1000 of the quad, i.e. a colour error far under one 8-bit quantum across even a 1920px span —
// while a highp coordinate promotes the three mixes to fp32 and, measured on the television
// (`plxnative-hwcnt`, 2026-09-02), that alone priced the hero's corner scrim at ~4.5 arithmetic
// words a fragment, 3.2M GPU cycles of a 11.7M-cycle frame. Banding on an opaque ground is an
// OUTPUT-quantisation problem and the noise is its cure; the varying was never the cause.
//
// DITHER: framebuffer GL_DITHER is intentionally disabled globally because its ordered dot pattern
// damaged shadows and rounded edges. An opaque ambient field still needs ±half an 8-bit quantum of
// unstructured noise or its deliberately slow gradient bands. `draw_ambient` enables that one-code
// dither; `draw_grad4` sets u_noise to zero because its alpha gradient is a scrim, not a ground.
//
// COST, measured on the television (Mali-T820, `plxnative-hwcnt` + `plxnative-drawmask=grad`,
// 2026-09-02): the first version of this dither was the textbook `fract(sin(dot(p, k)) * 43758.5)`
// and it ran UNCONDITIONALLY, `u_noise` only scaling the result. `sin` is a range reduction plus a
// polynomial on this part, so every fragment of every gradient paid ~7 arithmetic words for a value
// it then multiplied by zero: the hero's corner scrim went from 0.025 to ~3.7 GPU cycles per pixel
// — 5.3M of a 13.8M-cycle Home frame, 38% of it — and Hero paging fell to 46 fps, the hero→shelf
// fold to 38. Two rules fall out of it, and both are load-bearing rather than tidy:
//   * the noise is behind a UNIFORM branch — Midgard resolves that per draw, not per fragment —
//     so a scrim pays nothing for a ground's dither;
//   * the ground's noise is a TEXTURE FETCH (a 64x64 white-noise tile, `gfx::noise_tex`), not a
//     hash: an interleaved-gradient hash was tried in between and, in highp because
//     `gl_FragCoord` is, still cost the fold's full-screen wash ~2 cycles a pixel — 4M of a 14.4M
//     frame. The arithmetic pipe is what binds here; the texture pipe beside it is idle.
precision mediump float;
varying vec2 v_uv;
varying vec4 v_top;
varying vec4 v_bot;
uniform float u_noise;
uniform sampler2D u_noise_tex; // a 64x64 white-noise tile, GL_REPEAT + GL_NEAREST (gfx::noise_tex)
void main(){
  vec4 outc = mix(v_top, v_bot, v_uv.y);
  if (u_noise > 0.0) {
    outc.rgb += (texture2D(u_noise_tex, gl_FragCoord.xy * (1.0 / 64.0)).r - 0.5) * u_noise;
  }
  gl_FragColor = outc;
}
