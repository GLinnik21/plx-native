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
// damaged shadows and rounded edges. An opaque ambient field still needs unstructured noise or its
// deliberately slow gradient bands. `draw_ambient` enables that one-code dither at rest; in flight,
// and always for `draw_grad4` (whose alpha gradient is a scrim over ARTWORK, not a ground the eye
// rests on — dithering it would be adding grain to a photograph), this same source is linked behind
// `shaders/dither_stub.glsl` instead, a twin program with no uniform and no branch at all
// (`gfx::ambient_program`, 2026-09-04).
//
// **This shader's dither is `dither.glsl`'s now** (2026-09-02). It was written here first and every
// other slow gradient in the app either had a worse answer or none — `fs_glass.frag`, the popover
// background, had a `fract(sin(dot(…)))` hash running UNCONDITIONALLY, which is the exact mistake
// this file's own COST note records having made and fixed. The whole of the reasoning, the three
// measured cost rules and the tile's size argument moved to that prelude; nothing about the picture
// this program produces changed, and `gfx.rs`'s shader test still pins the divisor to `NOISE_DIM`.
precision mediump float;
varying vec2 v_uv;
varying vec4 v_top;
varying vec4 v_bot;
void main(){
  vec4 outc = mix(v_top, v_bot, v_uv.y);
  outc.rgb = plx_dither(outc.rgb);
  gl_FragColor = outc;
}
